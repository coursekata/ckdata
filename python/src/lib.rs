use pyo3::prelude::*;

mod bridge;
mod column_policy;
mod test_support;

/// The core version, then the Athena and S3 SDK versions.
#[pyfunction]
#[pyo3(name = "_core_versions")]
fn core_versions() -> Vec<String> {
    ckdata::versions()
}

#[pymodule]
#[pyo3(name = "_native")]
fn ckdata_py(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_function(wrap_pyfunction!(core_versions, m)?)?;
    m.add_function(wrap_pyfunction!(test_support::arrow_fixture, m)?)?;
    bridge::register(m)?;
    m.add_function(wrap_pyfunction!(column_policy::evaluate, m)?)?;
    m.add_function(wrap_pyfunction!(test_support::pending_job, m)?)?;
    Ok(())
}
