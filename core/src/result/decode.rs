use std::collections::HashSet;
use std::sync::Arc;

use arrow_array::{Array, BooleanArray, RecordBatch};
use arrow_schema::{DataType, Fields, SchemaRef};
use arrow_select::filter::filter_record_batch;
use bytes::Bytes;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use super::{Budget, ResultError, ResultLimits, reserve};
use crate::query::RequestGuard;

pub(super) struct Decoded {
    pub(super) schema: SchemaRef,
    pub(super) batches: Vec<RecordBatch>,
    pub(super) sentinels: usize,
}

pub(super) fn decode(
    bytes: Bytes,
    marker: &str,
    limits: &ResultLimits,
    budget: &Budget,
    guard: &RequestGuard<'_>,
) -> Result<Decoded, ResultError> {
    guard.check().map_err(ResultError::Interrupted)?;
    // Bound footer parsing before asking Parquet to construct its schema.
    if bytes.len() < 12 || &bytes[bytes.len() - 4..] != b"PAR1" {
        return Err(ResultError::InvalidParquet("missing footer"));
    }
    let footer = u32::from_le_bytes(
        bytes[bytes.len() - 8..bytes.len() - 4]
            .try_into()
            .expect("four-byte footer length"),
    ) as usize;
    if footer > 1024 * 1024 || footer > bytes.len() - 8 {
        return Err(ResultError::InvalidParquet("invalid or oversized footer"));
    }
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes)
        .map_err(|_| ResultError::InvalidParquet("cannot read metadata"))?;
    let declared_rows = usize::try_from(builder.metadata().file_metadata().num_rows())
        .map_err(|_| ResultError::InvalidParquet("invalid row count"))?;
    // The SQL wrapper can add at most one presence-only row per result. Count
    // that in the preflight budget without reducing the caller's real-row limit.
    reserve(
        &budget.rows,
        declared_rows,
        limits.max_rows.saturating_add(1),
        "row count",
    )?;
    let mut uncompressed = 0usize;
    for group in builder.metadata().row_groups() {
        let size = usize::try_from(group.total_byte_size())
            .map_err(|_| ResultError::InvalidParquet("invalid uncompressed size"))?;
        uncompressed = uncompressed
            .checked_add(size)
            .ok_or(ResultError::LimitExceeded("uncompressed bytes"))?;
    }
    reserve(
        &budget.uncompressed,
        uncompressed,
        limits.max_decoded_bytes,
        "uncompressed bytes",
    )?;
    let input_schema = builder.schema();
    validate_fields(input_schema.fields(), 0)?;
    let marker_index = input_schema
        .fields()
        .len()
        .checked_sub(1)
        .ok_or(ResultError::InvalidSchema("missing presence column"))?;
    if marker_index == 0
        || input_schema.field(marker_index).name() != marker
        || input_schema.field(marker_index).data_type() != &DataType::Boolean
    {
        return Err(ResultError::InvalidSchema(
            "missing or invalid presence column",
        ));
    }
    let projected: Vec<_> = (0..marker_index).collect();
    let schema = Arc::new(
        input_schema
            .project(&projected)
            .map_err(|_| ResultError::InvalidSchema("cannot project result"))?,
    );
    let reader = builder
        .with_batch_size(limits.batch_size)
        .build()
        .map_err(|_| ResultError::InvalidParquet("cannot build reader"))?;
    let mut batches = Vec::new();
    let mut sentinels = 0;
    for batch in reader {
        guard.check().map_err(ResultError::Interrupted)?;
        let batch = batch.map_err(|_| ResultError::InvalidParquet("cannot decode batch"))?;
        let presence = batch
            .column(marker_index)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .ok_or(ResultError::InvalidSchema("invalid presence column"))?;
        for row in 0..batch.num_rows() {
            if presence.is_null(row) {
                if batch.columns()[..marker_index]
                    .iter()
                    .any(|column| !column.is_null(row))
                {
                    return Err(ResultError::InvalidParquet("empty row contains data"));
                }
                sentinels += 1;
            } else if !presence.value(row) {
                return Err(ResultError::InvalidParquet("invalid presence value"));
            }
        }
        let projected = batch
            .project(&projected)
            .map_err(|_| ResultError::InvalidSchema("cannot project result"))?;
        let keep =
            BooleanArray::from_iter((0..batch.num_rows()).map(|row| Some(presence.is_valid(row))));
        let batch = filter_record_batch(&projected, &keep)
            .map_err(|_| ResultError::InvalidParquet("cannot remove presence row"))?;
        reserve(
            &budget.decoded,
            batch.get_array_memory_size(),
            limits.max_decoded_bytes,
            "decoded bytes",
        )?;
        if batch.num_rows() > 0 {
            batches.push(batch);
        }
    }
    Ok(Decoded {
        schema,
        batches,
        sentinels,
    })
}

fn validate_fields(fields: &Fields, depth: usize) -> Result<(), ResultError> {
    if depth > 32 {
        return Err(ResultError::UnsupportedType(
            "nested type exceeds depth limit",
        ));
    }
    let mut names = HashSet::new();
    for field in fields {
        if !names.insert(field.name().to_ascii_lowercase()) {
            return Err(ResultError::InvalidSchema("duplicate column names"));
        }
        validate_type(field.data_type(), depth)?;
    }
    Ok(())
}

fn validate_type(data_type: &DataType, depth: usize) -> Result<(), ResultError> {
    if depth > 32 {
        return Err(ResultError::UnsupportedType(
            "nested type exceeds depth limit",
        ));
    }
    match data_type {
        DataType::Timestamp(_, Some(_)) => {
            Err(ResultError::UnsupportedType("timezone-bearing timestamp"))
        }
        DataType::Struct(fields) => validate_fields(fields, depth + 1),
        DataType::List(field)
        | DataType::LargeList(field)
        | DataType::ListView(field)
        | DataType::LargeListView(field)
        | DataType::FixedSizeList(field, _)
        | DataType::Map(field, _) => validate_type(field.data_type(), depth + 1),
        DataType::Dictionary(_, value) => validate_type(value, depth + 1),
        _ => Ok(()),
    }
}
