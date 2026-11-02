use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3_arrow::PyRecordBatchReader;

/// The core's version, then the version of each AWS SDK crate it links.
#[pyfunction]
#[pyo3(name = "_core_versions")]
fn core_versions() -> Vec<String> {
    ckdata::versions()
}

/// The core's literal probe as an Arrow stream that Polars, pyarrow, pandas and DuckDB each accept.
#[pyfunction]
#[pyo3(name = "_arrow_probe")]
fn arrow_probe() -> PyResult<PyRecordBatchReader> {
    let reader = ckdata::arrow_probe().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    Ok(PyRecordBatchReader::new(reader))
}

#[pymodule]
#[pyo3(name = "ckdata")]
fn ckdata_py(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_function(wrap_pyfunction!(core_versions, m)?)?;
    m.add_function(wrap_pyfunction!(arrow_probe, m)?)?;
    Ok(())
}
