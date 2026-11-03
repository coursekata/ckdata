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

pub(super) fn decode(bytes: Bytes, guard: &RequestGuard<'_>) -> Result<Decoded, ResultError> {
    guard.check().map_err(ResultError::Interrupted)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes)
        .map_err(|_| ResultError::InvalidParquet("cannot read metadata"))?;
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
