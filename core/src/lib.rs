//! Shared connection and Arrow infrastructure for ckdata's R and Python bindings.
//! `Connection::connect` explicitly resolves AWS credentials and may assume a role.
//! The literal Arrow probe remains entirely offline.
mod connection;
mod error;
mod metadata;
mod query;
mod settings;

pub use connection::{Connection, SessionStatus};
pub use error::Error;
pub use metadata::{ColumnMetadata, TableMetadata, TablePage};
pub use query::{CancellationStatus, QueryExecution, QueryInterruption, QueryOptions};
pub use settings::ConnectionOptions;
pub use tokio_util::sync::CancellationToken as QueryCancellation;

use std::sync::Arc;

use arrow_array::types::TimestampMillisecondType;
use arrow_array::{
    ArrayRef, Int32Array, Int64Array, PrimitiveArray, RecordBatch, RecordBatchReader, StringArray,
};
use arrow_schema::ArrowError;
use bytes::Bytes;
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

/// The core's version, then the version of each AWS SDK crate it links.
pub fn versions() -> Vec<String> {
    vec![
        format!("ckdata {}", env!("CARGO_PKG_VERSION")),
        format!("aws-sdk-athena {}", aws_sdk_athena::meta::PKG_VERSION),
        format!("aws-sdk-s3 {}", aws_sdk_s3::meta::PKG_VERSION),
    ]
}

/// A literal table, round-tripped through Parquet the way UNLOAD output is read: a NULL string, an
/// empty string, a 64-bit integer past 2^53 and a naive timestamp.
pub fn arrow_probe() -> Result<Box<dyn RecordBatchReader + Send>, ArrowError> {
    let columns: Vec<(&str, ArrayRef)> = vec![
        ("id", Arc::new(Int32Array::from(vec![1, 2]))),
        ("s_null", Arc::new(StringArray::from(vec![None, Some("a")]))),
        (
            "s_empty",
            Arc::new(StringArray::from(vec![Some(""), Some("b")])),
        ),
        (
            "big",
            Arc::new(Int64Array::from(vec![Some(9_007_199_254_740_993), None])),
        ),
        (
            "ts",
            Arc::new(PrimitiveArray::<TimestampMillisecondType>::from(vec![
                Some(1_792_154_096_789),
                None,
            ])),
        ),
    ];
    let batch = RecordBatch::try_from_iter(columns)?;
    let mut parquet = Vec::new();
    let mut writer = ArrowWriter::try_new(&mut parquet, batch.schema(), None)?;
    writer.write(&batch)?;
    writer.close()?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(parquet))?.build()?;
    Ok(Box::new(reader))
}
