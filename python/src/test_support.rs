//! Offline Arrow fixture for compiled Python handoff tests.
use std::sync::Arc;

use arrow_array::types::TimestampMillisecondType;
use arrow_array::{
    ArrayRef, Int32Array, Int64Array, PrimitiveArray, RecordBatch, RecordBatchIterator, StringArray,
};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3_arrow::PyRecordBatchReader;

/// Invented values for testing the Python Arrow boundary without AWS credentials.
#[pyfunction]
#[pyo3(name = "_arrow_fixture")]
pub(super) fn arrow_fixture() -> PyResult<PyRecordBatchReader> {
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
    let batch =
        RecordBatch::try_from_iter(columns).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let schema = batch.schema();
    let reader = RecordBatchIterator::new(std::iter::once(Ok(batch)), schema);
    Ok(PyRecordBatchReader::new(Box::new(reader)))
}

/// A pending worker for exercising Python interrupts without AWS access.
#[pyfunction]
#[pyo3(name = "_pending_job")]
pub(super) fn pending_job() -> PyResult<super::bridge::NativeJob> {
    let cancellation = ckdata::QueryCancellation::new();
    let cancel = cancellation.clone();
    super::bridge::spawn(cancellation, false, async move {
        cancel.cancelled().await;
        Err(ckdata::Error::QueryInterrupted {
            query_id: Some("synthetic".into()),
            reason: ckdata::QueryInterruption::Cancelled,
            cancellation: ckdata::CancellationStatus::Requested,
        })
    })
}
