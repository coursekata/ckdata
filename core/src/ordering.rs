use arrow_array::RecordBatch;
use arrow_ord::sort::{SortColumn, SortOptions, lexsort_to_indices};
use arrow_schema::{DataType, Schema};
use arrow_select::{concat::concat_batches, take::take_record_batch};
use sqlparser::ast::{Expr, OrderByKind, OrderBySort, Query, SelectItem, SetExpr, Value};

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

pub(crate) fn sort(
    batches: Vec<RecordBatch>,
    keys: &[SortKey],
) -> Result<Vec<RecordBatch>, ResultError> {
    let schema = batches
        .first()
        .ok_or(ResultError::InvalidSchema("missing sort input"))?
        .schema();
    let combined = concat_batches(&schema, &batches)
        .map_err(|_| ResultError::InvalidSchema("cannot concatenate sort input"))?;
    drop(batches);
    let columns: Vec<_> = keys
        .iter()
        .map(|key| SortColumn {
            values: combined.column(key.index).clone(),
            options: Some(key.options),
        })
        .collect();
    let indices = lexsort_to_indices(&columns, None)
        .map_err(|_| ResultError::InvalidSchema("cannot sort returned columns"))?;
    let sorted = take_record_batch(&combined, &indices)
        .map_err(|_| ResultError::InvalidSchema("cannot apply result order"))?;
    Ok(vec![sorted])
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
    use std::sync::Arc;
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
        let sorted = sort(vec![first, second], &keys).unwrap();
        let values = sorted[0]
            .column(1)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(values.values().as_ref(), &[3, 1, 2, 1, 9]);
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
