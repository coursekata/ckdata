use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;
use bytes::Bytes;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use super::ResultError;
use crate::query::RequestGuard;

pub(super) struct Decoded {
    pub(super) schema: SchemaRef,
    pub(super) batches: Vec<RecordBatch>,
}

fn reader_builder(
    bytes: Bytes,
    guard: &RequestGuard<'_>,
) -> Result<ParquetRecordBatchReaderBuilder<Bytes>, ResultError> {
    guard.check().map_err(ResultError::Interrupted)?;
    ParquetRecordBatchReaderBuilder::try_new(bytes)
        .map_err(|_| ResultError::InvalidParquet("cannot read metadata"))
}

pub(super) fn schema(bytes: Bytes, guard: &RequestGuard<'_>) -> Result<SchemaRef, ResultError> {
    Ok(reader_builder(bytes, guard)?.schema().clone())
}

pub(super) fn decode(bytes: Bytes, guard: &RequestGuard<'_>) -> Result<Decoded, ResultError> {
    let builder = reader_builder(bytes, guard)?;
    let schema = builder.schema().clone();
    let reader = builder
        .build()
        .map_err(|_| ResultError::InvalidParquet("cannot build reader"))?;
    let mut batches = Vec::new();
    for batch in reader {
        guard.check().map_err(ResultError::Interrupted)?;
        batches.push(batch.map_err(|_| ResultError::InvalidParquet("cannot decode batch"))?);
    }
    Ok(Decoded { schema, batches })
}
