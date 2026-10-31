use pyo3::prelude::*;
use pyo3_arrow::PyRecordBatchReader;

#[pyfunction]
fn versions() -> Vec<String> {
    ckcore::versions()
}

/// The literal probe as an Arrow stream that Polars, pyarrow, pandas and DuckDB each accept.
#[pyfunction]
fn probe() -> PyRecordBatchReader {
    PyRecordBatchReader::new(ckcore::probe_reader())
}

#[pymodule]
fn ckspike(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(versions, m)?)?;
    m.add_function(wrap_pyfunction!(probe, m)?)?;
    Ok(())
}
