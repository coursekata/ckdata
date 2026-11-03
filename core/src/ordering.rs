use std::sync::Arc;

use arrow_array::{ArrayRef, Float16Array, Float32Array, Float64Array, RecordBatch};
use arrow_ord::ord::make_comparator;
use arrow_ord::sort::{SortColumn, SortOptions, lexsort_to_indices};
use arrow_schema::{DataType, Schema};
use arrow_select::{interleave::interleave_record_batch, take::take_record_batch};
use sqlparser::ast::{Expr, OrderByKind, OrderBySort, Query, SelectItem, SetExpr, Value};

use crate::query::RequestGuard;
use crate::{QueryWarning, ResultError};

pub(crate) struct SortKey {
    index: usize,
    options: SortOptions,
}

pub(crate) fn plan(query: &Query, schema: &Schema) -> Result<Vec<SortKey>, QueryWarning> {
    let query = outer_query(query);
    let Some(order) = &query.order_by else {
        return Ok(vec![]);
    };
    let OrderByKind::Expressions(expressions) = &order.kind else {
        return Err(warning("unsupported ORDER BY form"));
    };
    if order.interpolate.is_some() {
        return Err(warning("unsupported ORDER BY modifier"));
    }
    let mut keys = Vec::new();
    for order in expressions {
        if order.with_fill.is_some() || matches!(order.options.sort, Some(OrderBySort::Using(_))) {
            return Err(warning("unsupported ORDER BY modifier"));
        }
        let index = output_index(&order.expr, query, schema).ok_or(warning(
            "ORDER BY expression is not an unambiguous returned column",
        ))?;
        if !sortable(schema.field(index).data_type()) {
            return Err(warning(
                "ORDER BY type has no supported local sort semantics",
            ));
        }
        keys.push(SortKey {
            index,
            options: SortOptions {
                descending: matches!(order.options.sort, Some(OrderBySort::Desc)),
                // Athena/Trino defaults to NULLS LAST for both directions.
                nulls_first: order.options.nulls_first.unwrap_or(false),
            },
        });
    }
    Ok(keys)
}

const SORT_CHUNK_ROWS: usize = 4096;

pub(crate) fn sort(
    batches: Vec<RecordBatch>,
    keys: &[SortKey],
    guard: &RequestGuard<'_>,
) -> Result<Vec<RecordBatch>, ResultError> {
    sort_checked(batches, keys, &|| {
        guard.check().map_err(ResultError::Interrupted)
    })
}

fn sort_checked(
    batches: Vec<RecordBatch>,
    keys: &[SortKey],
    check: &impl Fn() -> Result<(), ResultError>,
) -> Result<Vec<RecordBatch>, ResultError> {
    let schema = batches
        .first()
        .ok_or(ResultError::InvalidSchema("missing sort input"))?
        .schema();
    let mut runs = Vec::new();
    for batch in batches {
        for offset in (0..batch.num_rows()).step_by(SORT_CHUNK_ROWS) {
            check()?;
            let batch = batch.slice(offset, SORT_CHUNK_ROWS.min(batch.num_rows() - offset));
            let columns: Vec<_> = keys
                .iter()
                .map(|key| SortColumn {
                    values: sort_values(batch.column(key.index)),
                    options: Some(key.options),
                })
                .collect();
            let indices = lexsort_to_indices(&columns, None)
                .map_err(|_| ResultError::InvalidSchema("cannot sort returned columns"))?;
            check()?;
            let sorted = take_record_batch(&batch, &indices)
                .map_err(|_| ResultError::InvalidSchema("cannot apply result order"))?;
            runs.push(vec![sorted]);
        }
    }
    // Merge bounded chunks rather than running one uninterruptible table sort.
    // Original arrays are materialized only in chunks; normalized float keys are
    // temporary and never become output columns.
    while runs.len() > 1 {
        let mut next = Vec::with_capacity(runs.len().div_ceil(2));
        let mut pairs = runs.into_iter();
        while let Some(left) = pairs.next() {
            check()?;
            next.push(match pairs.next() {
                Some(right) => merge_runs(left, right, keys, check)?,
                None => left,
            });
        }
        runs = next;
    }
    check()?;
    Ok(runs
        .pop()
        .unwrap_or_else(|| vec![RecordBatch::new_empty(schema)]))
}

fn merge_runs(
    left: Vec<RecordBatch>,
    right: Vec<RecordBatch>,
    keys: &[SortKey],
    check: &impl Fn() -> Result<(), ResultError>,
) -> Result<Vec<RecordBatch>, ResultError> {
    let mut output = Vec::new();
    let (mut left_batch, mut right_batch, mut left_row, mut right_row) = (0, 0, 0, 0);
    while left_batch < left.len() && right_batch < right.len() {
        check()?;
        let a = &left[left_batch];
        let b = &right[right_batch];
        let comparators: Vec<_> = keys
            .iter()
            .map(|key| {
                make_comparator(
                    sort_values(a.column(key.index)).as_ref(),
                    sort_values(b.column(key.index)).as_ref(),
                    key.options,
                )
                .map_err(|_| ResultError::InvalidSchema("cannot compare returned columns"))
            })
            .collect::<Result<_, _>>()?;
        let mut indices = Vec::with_capacity(SORT_CHUNK_ROWS);
        while left_row < a.num_rows() && right_row < b.num_rows() && indices.len() < SORT_CHUNK_ROWS
        {
            let order = comparators
                .iter()
                .map(|compare| compare(left_row, right_row))
                .find(|order| !order.is_eq())
                .unwrap_or(std::cmp::Ordering::Equal);
            if order.is_gt() {
                indices.push((1, right_row));
                right_row += 1;
            } else {
                indices.push((0, left_row));
                left_row += 1;
            }
        }
        check()?;
        output.push(
            interleave_record_batch(&[a, b], &indices)
                .map_err(|_| ResultError::InvalidSchema("cannot merge result order"))?,
        );
        if left_row == a.num_rows() {
            left_batch += 1;
            left_row = 0;
        }
        if right_row == b.num_rows() {
            right_batch += 1;
            right_row = 0;
        }
    }
    for (batches, start, row) in [
        (&left, left_batch, left_row),
        (&right, right_batch, right_row),
    ] {
        for (index, batch) in batches.iter().enumerate().skip(start) {
            check()?;
            let offset = if index == start { row } else { 0 };
            output.push(batch.slice(offset, batch.num_rows() - offset));
        }
    }
    Ok(output)
}

// Arrow uses IEEE totalOrder, which distinguishes signed zero and NaN bit
// patterns. Trino treats both zeros as equal and all NaNs as equal, above every
// numeric value. Normalize only temporary keys so the returned values retain
// their original bits and secondary ORDER BY keys break those SQL ties.
fn sort_values(values: &ArrayRef) -> ArrayRef {
    match values.data_type() {
        DataType::Float16 => {
            let values = values.as_any().downcast_ref::<Float16Array>().unwrap();
            Arc::new(Float32Array::from_iter(
                values
                    .iter()
                    .map(|value| value.map(|value| sql_f32(value.to_f32()))),
            ))
        }
        DataType::Float32 => {
            let values = values.as_any().downcast_ref::<Float32Array>().unwrap();
            Arc::new(Float32Array::from_iter(
                values.iter().map(|value| value.map(sql_f32)),
            ))
        }
        DataType::Float64 => {
            let values = values.as_any().downcast_ref::<Float64Array>().unwrap();
            Arc::new(Float64Array::from_iter(values.iter().map(|value| {
                value.map(|value| {
                    if value.is_nan() {
                        f64::NAN
                    } else if value == 0.0 {
                        0.0
                    } else {
                        value
                    }
                })
            })))
        }
        _ => values.clone(),
    }
}

fn sql_f32(value: f32) -> f32 {
    if value.is_nan() {
        f32::NAN
    } else if value == 0.0 {
        0.0
    } else {
        value
    }
}

fn warning(reason: &'static str) -> QueryWarning {
    QueryWarning::OrderNotRestored { reason }
}

fn outer_query(mut query: &Query) -> &Query {
    while query.order_by.is_none() {
        match query.body.as_ref() {
            SetExpr::Query(inner) => query = inner,
            _ => break,
        }
    }
    query
}

fn output_index(expr: &Expr, query: &Query, schema: &Schema) -> Option<usize> {
    if let Expr::Value(value) = expr
        && let Value::Number(ordinal, _) = &value.value
    {
        return ordinal
            .parse::<usize>()
            .ok()?
            .checked_sub(1)
            .filter(|index| *index < schema.fields().len());
    }
    // ORDER BY names resolve to output aliases before source-column names.
    if let Expr::Identifier(name) = expr {
        let matches: Vec<_> = schema
            .fields()
            .iter()
            .enumerate()
            .filter(|(_, field)| field.name().eq_ignore_ascii_case(&name.value))
            .collect();
        if matches.len() == 1 {
            return Some(matches[0].0);
        }
    }
    fn projection(body: &SetExpr) -> Option<&[SelectItem]> {
        match body {
            SetExpr::Select(select) => Some(&select.projection),
            SetExpr::Query(query) => projection(&query.body),
            SetExpr::SetOperation { left, .. } => projection(left),
            _ => None,
        }
    }
    let projection = projection(&query.body)?;
    if projection.len() != schema.fields().len()
        || projection.iter().any(|item| {
            !matches!(
                item,
                SelectItem::UnnamedExpr(_) | SelectItem::ExprWithAlias { .. }
            )
        })
    {
        return None;
    }
    let matches: Vec<_> = projection
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let candidate = match item {
                SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => expr,
                _ => return None,
            };
            (candidate.to_string() == expr.to_string()).then_some(index)
        })
        .collect();
    (matches.len() == 1).then(|| matches[0])
}

fn sortable(data_type: &DataType) -> bool {
    matches!(
        data_type,
        DataType::Null
            | DataType::Boolean
            | DataType::Int8
            | DataType::Int16
            | DataType::Int32
            | DataType::Int64
            | DataType::UInt8
            | DataType::UInt16
            | DataType::UInt32
            | DataType::UInt64
            | DataType::Float16
            | DataType::Float32
            | DataType::Float64
            | DataType::Utf8
            | DataType::LargeUtf8
            | DataType::Utf8View
            | DataType::Binary
            | DataType::LargeBinary
            | DataType::BinaryView
            | DataType::FixedSizeBinary(_)
            | DataType::Date32
            | DataType::Date64
            | DataType::Time32(_)
            | DataType::Time64(_)
            | DataType::Timestamp(_, None)
            | DataType::Decimal128(_, _)
            | DataType::Decimal256(_, _)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::read_query;
    use arrow_array::{Int64Array, StringArray};
    fn sort(batches: Vec<RecordBatch>, keys: &[SortKey]) -> Result<Vec<RecordBatch>, ResultError> {
        super::sort_checked(batches, keys, &|| Ok(()))
    }

    fn combined(batches: &[RecordBatch]) -> RecordBatch {
        arrow_select::concat::concat_batches(&batches[0].schema(), batches).unwrap()
    }

    #[test]
    fn sorts_multiple_shuffled_batches_with_directions_nulls_aliases_and_ordinals() {
        let first = RecordBatch::try_from_iter(vec![
            (
                "label",
                Arc::new(StringArray::from(vec![Some("b"), None, Some("a")])) as _,
            ),
            ("value", Arc::new(Int64Array::from(vec![1, 9, 1])) as _),
        ])
        .unwrap();
        let second = RecordBatch::try_from_iter(vec![
            (
                "label",
                Arc::new(StringArray::from(vec![Some("a"), Some("b")])) as _,
            ),
            ("value", Arc::new(Int64Array::from(vec![3, 2])) as _),
        ])
        .unwrap();
        let query = read_query("SELECT source AS label, n AS value FROM synthetic ORDER BY label ASC NULLS LAST, 2 DESC").unwrap();
        let keys = plan(&query, &first.schema()).unwrap();
        let sorted = [combined(&sort(vec![first, second], &keys).unwrap())];
        let values = sorted[0]
            .column(1)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(values.values().as_ref(), &[3, 1, 2, 1, 9]);
    }

    #[test]
    fn floats_use_sql_ties_and_preserve_original_bits() {
        use arrow_array::types::{ArrowPrimitiveType, Float16Type};
        type F16 = <Float16Type as ArrowPrimitiveType>::Native;
        let inputs: Vec<ArrayRef> = vec![
            Arc::new(Float16Array::from(vec![
                Some(F16::from_bits(0x8000)),
                Some(F16::from_bits(0x0000)),
                Some(F16::from_bits(0x7e01)),
                Some(F16::from_bits(0xfe02)),
                None,
                Some(F16::NEG_INFINITY),
                Some(F16::from_f32(-1.0)),
                Some(F16::INFINITY),
            ])),
            Arc::new(Float32Array::from(vec![
                Some(-0.0),
                Some(0.0),
                Some(f32::from_bits(0x7fc0_0001)),
                Some(f32::from_bits(0xffc0_0002)),
                None,
                Some(f32::NEG_INFINITY),
                Some(-1.0),
                Some(f32::INFINITY),
            ])),
            Arc::new(Float64Array::from(vec![
                Some(-0.0),
                Some(0.0),
                Some(f64::from_bits(0x7ff8_0000_0000_0001)),
                Some(f64::from_bits(0xfff8_0000_0000_0002)),
                None,
                Some(f64::NEG_INFINITY),
                Some(-1.0),
                Some(f64::INFINITY),
            ])),
        ];
        for values in inputs {
            let batch = RecordBatch::try_from_iter(vec![
                ("f", values.clone()),
                (
                    "id",
                    Arc::new(Int64Array::from(vec![2, 1, 4, 3, 5, 7, 8, 9])) as _,
                ),
            ])
            .unwrap();
            for (order, expected) in [
                ("ASC", vec![7, 8, 1, 2, 9, 3, 4, 5]),
                ("DESC", vec![3, 4, 9, 1, 2, 8, 7, 5]),
                ("ASC NULLS FIRST", vec![5, 7, 8, 1, 2, 9, 3, 4]),
                ("DESC NULLS FIRST", vec![5, 3, 4, 9, 1, 2, 8, 7]),
            ] {
                let query = read_query(&format!(
                    "SELECT f, id FROM synthetic ORDER BY f {order}, id ASC"
                ))
                .unwrap();
                let keys = plan(&query, &batch.schema()).unwrap();
                let sorted = [combined(
                    &sort(vec![batch.slice(0, 4), batch.slice(4, 4)], &keys).unwrap(),
                )];
                let ids = sorted[0]
                    .column(1)
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .unwrap();
                assert_eq!(
                    ids.iter().collect::<Vec<_>>(),
                    expected.iter().copied().map(Some).collect::<Vec<_>>()
                );
                // Compare the original representation after selecting rows by ID.
                // This catches normalizing returned values as well as sort keys.
                for (row, id) in expected.into_iter().enumerate() {
                    let input = [2, 1, 4, 3, 5, 7, 8, 9]
                        .iter()
                        .position(|value| *value == id)
                        .unwrap();
                    let actual = sorted[0].column(0);
                    assert_eq!(actual.is_null(row), values.is_null(input));
                    if values.is_null(input) {
                        continue;
                    }
                    macro_rules! bits {
                        ($ty:ty) => {
                            assert_eq!(
                                actual
                                    .as_any()
                                    .downcast_ref::<$ty>()
                                    .unwrap()
                                    .value(row)
                                    .to_bits(),
                                values
                                    .as_any()
                                    .downcast_ref::<$ty>()
                                    .unwrap()
                                    .value(input)
                                    .to_bits()
                            )
                        };
                    }
                    match values.data_type() {
                        DataType::Float16 => bits!(Float16Array),
                        DataType::Float32 => bits!(Float32Array),
                        DataType::Float64 => bits!(Float64Array),
                        _ => unreachable!(),
                    }
                }
            }
        }
    }
    #[test]
    fn bounded_sort_merges_every_row_and_checks_between_work_chunks() {
        let batch = RecordBatch::try_from_iter(vec![(
            "n",
            Arc::new(Int64Array::from_iter_values((0..10_001).rev())) as ArrayRef,
        )])
        .unwrap();
        let keys = plan(
            &read_query("SELECT n FROM synthetic ORDER BY n").unwrap(),
            &batch.schema(),
        )
        .unwrap();
        let sorted = sort(vec![batch.clone()], &keys).unwrap();
        assert!(
            sorted
                .iter()
                .all(|batch| batch.num_rows() <= SORT_CHUNK_ROWS)
        );
        let values: Vec<_> = sorted
            .iter()
            .flat_map(|batch| {
                batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .unwrap()
                    .iter()
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(values, (0..10_001).map(Some).collect::<Vec<_>>());
        // The first point interrupts the second local run; the second reaches
        // the merge pass. Neither should wait for all rows to be sorted/copied.
        for stop_at in [3, 9] {
            let checks = std::cell::Cell::new(0);
            let result = super::sort_checked(vec![batch.clone()], &keys, &|| {
                checks.set(checks.get() + 1);
                if checks.get() == stop_at {
                    Err(ResultError::Interrupted(
                        crate::QueryInterruption::Cancelled,
                    ))
                } else {
                    Ok(())
                }
            });
            assert!(matches!(
                result,
                Err(ResultError::Interrupted(
                    crate::QueryInterruption::Cancelled
                ))
            ));
            assert_eq!(checks.get(), stop_at);
        }
    }

    #[test]
    fn resolves_output_aliases_before_sources_and_does_not_guess_hidden_keys() {
        let schema = Schema::new(vec![
            arrow_schema::Field::new("y", DataType::Int64, true),
            arrow_schema::Field::new("z", DataType::Int64, true),
        ]);
        assert_eq!(
            plan(
                &read_query("SELECT x AS y, y AS z FROM t ORDER BY y").unwrap(),
                &schema
            )
            .unwrap()[0]
                .index,
            0
        );
        assert_eq!(
            plan(
                &read_query("SELECT t.x AS y, t.y AS z FROM t ORDER BY t.y").unwrap(),
                &schema
            )
            .unwrap()[0]
                .index,
            1
        );
        assert!(
            plan(
                &read_query("SELECT x AS y, a AS z FROM t ORDER BY hidden").unwrap(),
                &schema
            )
            .is_err()
        );
        assert!(
            plan(
                &read_query("SELECT * FROM t ORDER BY t.y").unwrap(),
                &schema
            )
            .is_err()
        );
        assert!(
            plan(
                &read_query("SELECT * FROM (SELECT * FROM t ORDER BY y) t").unwrap(),
                &schema
            )
            .unwrap()
            .is_empty()
        );
        assert!(
            !plan(
                &read_query("(SELECT x AS y, a AS z FROM t ORDER BY y DESC)").unwrap(),
                &schema
            )
            .unwrap()[0]
                .options
                .nulls_first
        );
    }
}
