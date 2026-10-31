//! Delivery spike for a shared ckdata core: links the real dependency tree and returns a literal
//! table through Parquet and Arrow. It makes no AWS call and holds no credentials.
use std::sync::Arc;

use arrow_array::types::TimestampMillisecondType;
use arrow_array::{
    ArrayRef, Int32Array, Int64Array, PrimitiveArray, RecordBatch, RecordBatchIterator, RecordBatchReader,
    StringArray,
};
use bytes::Bytes;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;

/// The SDK clients the real core uses, configured offline so the linker keeps them.
pub fn versions() -> Vec<String> {
    let region = aws_config::Region::new("us-west-2");
    let athena = aws_sdk_athena::config::Builder::new()
        .behavior_version(aws_config::BehaviorVersion::latest())
        .region(region.clone())
        .build();
    let s3 = aws_sdk_s3::config::Builder::new()
        .behavior_version(aws_config::BehaviorVersion::latest())
        .region(region)
        .build();
    vec![
        format!("ckcore {}", env!("CARGO_PKG_VERSION")),
        format!("athena client region {}", athena.region().map(|r| r.as_ref()).unwrap_or("none")),
        format!("s3 client region {}", s3.region().map(|r| r.as_ref()).unwrap_or("none")),
    ]
}

/// A literal table, round-tripped through Parquet the way UNLOAD output is read: a NULL string, an
/// empty string, a 64-bit integer past 2^53 and a naive timestamp.
pub fn probe_reader() -> Box<dyn RecordBatchReader + Send> {
    let cols: Vec<ArrayRef> = vec![
        Arc::new(Int32Array::from(vec![1, 2])),
        Arc::new(StringArray::from(vec![None, Some("a")])),
        Arc::new(StringArray::from(vec![Some(""), Some("b")])),
        Arc::new(Int64Array::from(vec![Some(9_007_199_254_740_993), None])),
        Arc::new(PrimitiveArray::<TimestampMillisecondType>::from(vec![Some(1_792_154_096_789), None])),
    ];
    let names = ["id", "s_null", "s_empty", "big", "ts"];
    let batch = RecordBatch::try_from_iter(names.iter().zip(cols).map(|(n, c)| (*n, c))).unwrap();
    let mut buf = Vec::new();
    let mut writer = ArrowWriter::try_new(&mut buf, batch.schema(), None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
    let reader = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(buf)).unwrap().build().unwrap();
    let schema = reader.schema();
    let batches: Vec<_> = reader.collect();
    Box::new(RecordBatchIterator::new(batches, schema))
}
