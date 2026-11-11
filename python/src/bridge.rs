//! Python polls owned Rust jobs; AWS work never holds the interpreter lock.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use ckdata::{Connection, ConnectionOptions, QueryCancellation, QueryOptions, SsoLogin};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3_arrow::PyRecordBatchReader;

pyo3::create_exception!(ckdata, CkDataError, PyRuntimeError);

type Outcome = Result<Completion, ckdata::Error>;
type RuntimeSlot = (u32, Mutex<Option<tokio::runtime::Runtime>>);
static RUNTIME: OnceLock<RuntimeSlot> = OnceLock::new();

#[pyclass(frozen, module = "ckdata._native")]
pub(super) struct NativeConnection {
    connection: Mutex<Option<Connection>>,
    options: ConnectionOptions,
    region: String,
    identity: ckdata::CallerIdentity,
    login_allowed: bool,
}

#[pyclass(frozen, module = "ckdata._native")]
pub(super) struct NativeJob {
    value: Arc<Mutex<Option<Outcome>>>,
    cancellation: QueryCancellation,
    task: tokio::task::JoinHandle<()>,
    login: SsoLogin,
}

impl Drop for NativeJob {
    fn drop(&mut self) {
        if RUNTIME
            .get()
            .is_some_and(|(pid, _)| *pid != std::process::id())
        {
            return;
        }
        // Dropping the handle leaves its worker scheduled to request a remote
        // stop, including when submission has not yet returned a query ID.
        self.cancellation.cancel();
        self.login.complete(false);
    }
}

pub(super) enum Completion {
    Connected(Box<NativeConnection>),
    Query(ckdata::QueryResult),
    Columns(Vec<(String, String)>),
    Tables(Vec<String>),
    Workgroup(WorkgroupInfo),
}

pub(super) struct WorkgroupInfo {
    output_location: Option<String>,
    expected_bucket_owner: Option<String>,
    encryption_option: Option<String>,
    kms_key: Option<String>,
    enforce_work_group_configuration: Option<bool>,
    bytes_scanned_cutoff_per_query: Option<i64>,
    selected_engine_version: Option<String>,
}

fn runtime_error(error: impl ToString) -> PyErr {
    PyRuntimeError::new_err(error.to_string())
}

fn check_process() -> PyResult<()> {
    if RUNTIME
        .get()
        .is_some_and(|(pid, _)| *pid != std::process::id())
    {
        return Err(CkDataError::new_err(
            "ckdata runtime was inherited by a forked process; use multiprocessing's spawn context and connect inside each worker",
        ));
    }
    Ok(())
}

pub(super) fn spawn(
    cancellation: QueryCancellation,
    login_allowed: bool,
    future: impl Future<Output = Outcome> + Send + 'static,
) -> PyResult<NativeJob> {
    let (_, slot) = RUNTIME.get_or_init(|| (std::process::id(), Mutex::new(None)));
    check_process()?;
    let mut runtime = slot.lock().map_err(runtime_error)?;
    if runtime.is_none() {
        *runtime = Some(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .map_err(runtime_error)?,
        );
    }
    let login = SsoLogin::new(login_allowed);
    let interaction = login.clone();
    let value = Arc::new(Mutex::new(None));
    let output = value.clone();
    let task = runtime
        .as_ref()
        .expect("runtime initialized")
        .spawn(async move {
            let result = interaction.scope(future).await;
            if let Ok(mut slot) = output.lock() {
                *slot = Some(result);
            }
        });
    Ok(NativeJob {
        value,
        cancellation,
        task,
        login,
    })
}

fn target_dict(py: Python<'_>, target: ckdata::SsoLoginTarget) -> PyResult<Bound<'_, PyDict>> {
    let result = PyDict::new(py);
    result.set_item("profile", target.profile)?;
    result.set_item("session", target.session)?;
    result.set_item("config", target.config)?;
    Ok(result)
}

fn native_error(py: Python<'_>, error: ckdata::Error, login: &SsoLogin) -> PyResult<PyErr> {
    let failure = CkDataError::new_err(error.to_string());
    let value = failure.value(py);
    let target = match &error {
        ckdata::Error::SsoLoginRequired { target } => Some(target.clone()),
        _ => login.required(),
    };
    value.setattr("login_disabled", target.is_some() && !login.allowed())?;
    value.setattr(
        "credentials_unavailable",
        matches!(error, ckdata::Error::CredentialsUnavailable),
    )?;
    value.setattr("remote_may_be_running", error.remote_query_may_be_running())?;
    value.setattr(
        "cancellation",
        error.cancellation_status().map(|status| match status {
            ckdata::CancellationStatus::NotSubmitted => "not_submitted",
            ckdata::CancellationStatus::Requested => "requested",
            ckdata::CancellationStatus::Unconfirmed => "unconfirmed",
        }),
    )?;
    value.setattr(
        "sso_login",
        target.map(|target| target_dict(py, target)).transpose()?,
    )?;
    Ok(failure)
}

#[pymethods]
impl NativeJob {
    fn ready(&self) -> PyResult<bool> {
        check_process()?;
        Ok(self.value.lock().map_err(runtime_error)?.is_some() || self.task.is_finished())
    }

    fn cancel(&self) -> PyResult<()> {
        check_process()?;
        self.cancellation.cancel();
        self.login.complete(false);
        Ok(())
    }

    fn login_target(&self, py: Python<'_>) -> PyResult<Option<Py<PyDict>>> {
        check_process()?;
        self.login
            .pending()
            .map(|target| target_dict(py, target).map(Bound::unbind))
            .transpose()
    }

    fn login_complete(&self, success: bool) -> PyResult<()> {
        check_process()?;
        self.login.complete(success);
        Ok(())
    }

    fn result(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        check_process()?;
        let outcome = self
            .value
            .lock()
            .map_err(runtime_error)?
            .take()
            .ok_or_else(|| {
                runtime_error("operation is not ready or its result was already consumed")
            })?;
        match outcome {
            Ok(value) => completion_value(py, value),
            Err(error) => Err(native_error(py, error, &self.login)?),
        }
    }
}

fn completion_value(py: Python<'_>, value: Completion) -> PyResult<Py<PyAny>> {
    Ok(match value {
        Completion::Connected(value) => Py::new(py, *value)?.into_any(),
        Completion::Query(value) => {
            let result = PyDict::new(py);
            result.set_item("query_id", value.query_id())?;
            result.set_item(
                "reader",
                Py::new(py, PyRecordBatchReader::new(Box::new(value.into_reader())))?,
            )?;
            result.into_any().unbind()
        }
        Completion::Columns(value) => value.into_pyobject(py)?.into_any().unbind(),
        Completion::Tables(value) => value.into_pyobject(py)?.into_any().unbind(),
        Completion::Workgroup(value) => {
            let result = PyDict::new(py);
            result.set_item("output_location", value.output_location)?;
            result.set_item("expected_bucket_owner", value.expected_bucket_owner)?;
            result.set_item("encryption_option", value.encryption_option)?;
            result.set_item("kms_key", value.kms_key)?;
            result.set_item(
                "enforce_work_group_configuration",
                value.enforce_work_group_configuration,
            )?;
            result.set_item(
                "bytes_scanned_cutoff_per_query",
                value.bytes_scanned_cutoff_per_query,
            )?;
            result.set_item("selected_engine_version", value.selected_engine_version)?;
            result.into_any().unbind()
        }
    })
}

fn setting(settings: &HashMap<String, Option<String>>, name: &str) -> Option<String> {
    settings.get(name).cloned().flatten()
}

#[pyfunction]
fn connect(
    settings: HashMap<String, Option<String>>,
    sso: HashMap<String, Option<String>>,
    login_allowed: bool,
) -> PyResult<NativeJob> {
    let required = |name| {
        setting(&settings, name).ok_or_else(|| PyValueError::new_err(format!("missing {name}")))
    };
    let mut options = ConnectionOptions::new(required("workgroup")?, required("schema")?);
    options.profile = setting(&settings, "profile");
    options.region = setting(&settings, "region");
    options.role_arn = setting(&settings, "role_arn");
    options.output_location = setting(&settings, "output_location");
    if !sso.is_empty() {
        options.sso = Some(ckdata::SsoOptions {
            start_url: setting(&sso, "start_url"),
            region: setting(&sso, "region"),
            account_id: setting(&sso, "account_id"),
            role_name: setting(&sso, "role_name"),
            session: setting(&sso, "session"),
            registration_scopes: setting(&sso, "registration_scopes"),
            fallback_profile: setting(&sso, "fallback_profile"),
        });
    }
    let cancellation = QueryCancellation::new();
    let cancel = cancellation.clone();
    spawn(cancellation, login_allowed, async move {
        tokio::select! {
            _ = cancel.cancelled() => Err(ckdata::Error::InvalidSetting("connection cancelled")),
            result = async {
                let connection = Connection::connect(options).await?;
                let identity = connection.caller_identity().await?;
                connection.workgroup_configuration().await?;
                Ok(Completion::Connected(Box::new(NativeConnection {
                    options: connection.options().clone(),
                    region: connection.region().to_owned(),
                    connection: Mutex::new(Some(connection)),
                    identity,
                    login_allowed,
                })))
            } => result
        }
    })
}

impl NativeConnection {
    fn cloned_connection(&self) -> PyResult<Connection> {
        check_process()?;
        self.connection
            .lock()
            .map_err(runtime_error)?
            .clone()
            .ok_or_else(|| CkDataError::new_err("ckdata: connection is closed"))
    }
}

#[pymethods]
impl NativeConnection {
    fn close(&self) -> PyResult<()> {
        check_process()?;
        self.connection.lock().map_err(runtime_error)?.take();
        Ok(())
    }

    #[getter]
    fn closed(&self) -> PyResult<bool> {
        check_process()?;
        Ok(self.connection.lock().map_err(runtime_error)?.is_none())
    }

    fn info(&self, py: Python<'_>) -> PyResult<Py<PyDict>> {
        let info = PyDict::new(py);
        info.set_item("account", &self.identity.account)?;
        info.set_item("arn", &self.identity.arn)?;
        info.set_item("region", &self.region)?;
        info.set_item("schema", &self.options.schema)?;
        info.set_item("workgroup", &self.options.workgroup)?;
        Ok(info.unbind())
    }

    #[pyo3(signature = (sql, timeout, parameters, schema_only=false))]
    fn query(
        &self,
        sql: String,
        timeout: f64,
        parameters: Vec<String>,
        schema_only: bool,
    ) -> PyResult<NativeJob> {
        if !timeout.is_finite() || timeout <= 0.0 || timeout > 86400.0 {
            return Err(PyValueError::new_err(
                "timeout must be greater than zero and at most 86400 seconds",
            ));
        }
        let connection = self.cloned_connection()?;
        let options = QueryOptions {
            timeout: Duration::from_secs_f64(timeout),
            execution_parameters: parameters,
            ..QueryOptions::default()
        };
        let cancellation = QueryCancellation::new();
        let cancel = cancellation.clone();
        spawn(cancellation, self.login_allowed, async move {
            let result = if schema_only {
                connection.query_empty(&sql, &options, &cancel).await
            } else {
                connection.query(&sql, &options, &cancel).await
            };
            result.map(Completion::Query)
        })
    }

    #[pyo3(signature = (schema, table=None))]
    fn metadata(&self, schema: String, table: Option<String>) -> PyResult<NativeJob> {
        let connection = self
            .cloned_connection()?
            .with_schema(&schema)
            .map_err(|error| CkDataError::new_err(error.to_string()))?;
        let cancellation = QueryCancellation::new();
        let cancel = cancellation.clone();
        spawn(cancellation, self.login_allowed, async move {
            tokio::select! {
                _ = cancel.cancelled() => Err(ckdata::Error::InvalidSetting("metadata request cancelled")),
                result = async {
                    if let Some(table) = table {
                        let table = connection.table_metadata(&table).await?;
                        return Ok(Completion::Columns(table.columns.into_iter().chain(table.partition_keys)
                            .map(|column| (column.name, column.data_type.unwrap_or_default())).collect()));
                    }
                    let mut tables = Vec::new();
                    let mut token = None;
                    loop {
                        let page = connection.table_page(token.as_deref()).await?;
                        tables.extend(page.tables.into_iter().map(|table| table.name));
                        token = page.next_token;
                        if token.is_none() { return Ok(Completion::Tables(tables)); }
                    }
                } => result
            }
        })
    }

    fn workgroup(&self) -> PyResult<NativeJob> {
        let connection = self.cloned_connection()?;
        let cancellation = QueryCancellation::new();
        let cancel = cancellation.clone();
        spawn(cancellation, self.login_allowed, async move {
            tokio::select! {
                _ = cancel.cancelled() => Err(ckdata::Error::InvalidSetting("workgroup request cancelled")),
                result = async {
                    let config = connection.workgroup_configuration().await?;
                    let output = config.result_configuration();
                    let encryption = output.and_then(|output| output.encryption_configuration());
                    Ok(Completion::Workgroup(WorkgroupInfo {
                        output_location: output.and_then(|output| output.output_location()).map(str::to_owned),
                        expected_bucket_owner: output.and_then(|output| output.expected_bucket_owner()).map(str::to_owned),
                        encryption_option: encryption.map(|value| value.encryption_option().as_str().to_owned()),
                        kms_key: encryption.and_then(|value| value.kms_key()).map(str::to_owned),
                        enforce_work_group_configuration: config.enforce_work_group_configuration,
                        bytes_scanned_cutoff_per_query: config.bytes_scanned_cutoff_per_query,
                        selected_engine_version: config.engine_version().and_then(|value| value.selected_engine_version()).map(str::to_owned),
                    }))
                } => result
            }
        })
    }
}

pub(super) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add("Error", module.py().get_type::<CkDataError>())?;
    module.add_class::<NativeConnection>()?;
    module.add_class::<NativeJob>()?;
    module.add_function(wrap_pyfunction!(connect, module)?)?;
    Ok(())
}
