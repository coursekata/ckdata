use arrow_array::RecordBatch;
use arrow_schema::{DataType, Field, Fields, Schema, SchemaRef, TimeUnit};
use bytes::Bytes;
use parquet::arrow::arrow_reader::{
    ArrowReaderMetadata, ArrowReaderOptions, ParquetRecordBatchReaderBuilder,
};
use parquet::basic::Type;
use parquet::schema::types::ColumnDescPtr;
use std::sync::Arc;

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
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes.clone())
        .map_err(|_| ResultError::InvalidParquet("cannot read metadata"))?;
    let columns = builder.parquet_schema().columns();
    if !columns
        .iter()
        .any(|column| column.physical_type() == Type::INT96)
    {
        return Ok(builder);
    }
    // Athena's INT96 timestamps need microseconds to retain its full date range.
    // Supply the upstream reader a type hint before decoding; casting already
    // decoded nanoseconds would retain the overflow. Other physical types stay intact.
    let mut columns = columns.iter();
    let fields: Fields = builder
        .schema()
        .fields()
        .iter()
        .map(|field| int96_field(field, &mut columns))
        .collect();
    let schema = Arc::new(Schema::new_with_metadata(
        fields,
        builder.schema().metadata().clone(),
    ));
    let metadata = ArrowReaderMetadata::try_new(
        builder.metadata().clone(),
        ArrowReaderOptions::new().with_schema(schema),
    )
    .map_err(|_| ResultError::InvalidParquet("cannot configure timestamp reader"))?;
    Ok(ParquetRecordBatchReaderBuilder::new_with_metadata(
        bytes, metadata,
    ))
}

fn int96_field(field: &Field, columns: &mut std::slice::Iter<'_, ColumnDescPtr>) -> Field {
    let data_type = match field.data_type() {
        DataType::Struct(fields) => DataType::Struct(
            fields
                .iter()
                .map(|field| int96_field(field, columns))
                .collect(),
        ),
        DataType::List(child) => DataType::List(Arc::new(int96_field(child, columns))),
        DataType::LargeList(child) => DataType::LargeList(Arc::new(int96_field(child, columns))),
        DataType::FixedSizeList(child, size) => {
            DataType::FixedSizeList(Arc::new(int96_field(child, columns)), *size)
        }
        DataType::Map(child, sorted) => {
            DataType::Map(Arc::new(int96_field(child, columns)), *sorted)
        }
        data_type => {
            let physical = columns.next().map(|column| column.physical_type());
            match (physical, data_type) {
                (Some(Type::INT96), DataType::Timestamp(TimeUnit::Nanosecond, timezone)) => {
                    DataType::Timestamp(TimeUnit::Microsecond, timezone.clone())
                }
                _ => data_type.clone(),
            }
        }
    };
    field.clone().with_data_type(data_type)
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
