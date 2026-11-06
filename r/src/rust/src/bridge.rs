//! R objects are created only on the R thread. Background jobs own Rust values
//! and communicate completion through a mutex; R can process interrupts while
//! the shared core completes a cancellation request.
use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use arrow_array::{RecordBatch, RecordBatchIterator, RecordBatchReader};
use arrow_schema::SchemaRef;
use ckdata::{Connection, ConnectionOptions, QueryCancellation, QueryOptions, SessionStatus};
use extendr_api::Result;
use extendr_api::prelude::*;

type NativeValue = std::result::Result<Completion, NativeFailure>;

struct NativeFailure {
    message: String,
    cancellation: Option<ckdata::CancellationStatus>,
    remote_may_be_running: bool,
}

impl NativeFailure {
    fn message(message: impl ToString) -> Self {
        Self {
            message: message.to_string(),
            cancellation: None,
            remote_may_be_running: false,
        }
    }

    fn into_list(self) -> List {
        let cancellation = self.cancellation.map(|status| match status {
            ckdata::CancellationStatus::NotSubmitted => "not_submitted",
            ckdata::CancellationStatus::Requested => "requested",
            ckdata::CancellationStatus::Unconfirmed => "unconfirmed",
        });
        list!(
            message = self.message,
            cancellation = cancellation.map_or_else(|| ().into_robj(), |value| value.into_robj()),
            remote_may_be_running = self.remote_may_be_running
        )
    }
}

impl From<ckdata::Error> for NativeFailure {
    fn from(error: ckdata::Error) -> Self {
        Self {
            message: error.to_string(),
            cancellation: error.cancellation_status(),
            remote_may_be_running: error.remote_query_may_be_running(),
        }
    }
}

struct NativeConnection {
    connection: Option<Connection>,
    options: ConnectionOptions,
    region: String,
    identity: ckdata::CallerIdentity,
}

struct NativeJob {
    value: Arc<Mutex<Option<NativeValue>>>,
    cancellation: Arc<QueryCancellation>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for NativeJob {
    fn drop(&mut self) {
        // The task remains scheduled so the core can obtain the query ID and
        // request remote cancellation even after R releases its job handle.
        self.cancellation.cancel();
    }
}

enum Completion {
    Connected(Box<NativeConnection>),
    Query(ckdata::QueryResult),
    Table(ckdata::TableMetadata),
    Tables(Vec<String>),
    Workgroup(WorkgroupInfo),
    Probe(NativeResult),
}

struct WorkgroupInfo {
    output_location: Option<String>,
    expected_bucket_owner: Option<String>,
    encryption_option: Option<String>,
    kms_key: Option<String>,
    enforce_work_group_configuration: Option<bool>,
    bytes_scanned_cutoff_per_query: Option<i64>,
    selected_engine_version: Option<String>,
}

impl WorkgroupInfo {
    fn into_list(self) -> List {
        list!(
            output_location = self.output_location,
            expected_bucket_owner = self.expected_bucket_owner,
            encryption_option = self.encryption_option,
            kms_key = self
                .kms_key
                .map_or_else(|| ().into_robj(), |key| key.into_robj()),
            enforce_work_group_configuration = self.enforce_work_group_configuration,
            bytes_scanned_cutoff_per_query = self.bytes_scanned_cutoff_per_query,
            selected_engine_version = self.selected_engine_version
        )
    }
}

struct NativeResult {
    schema: SchemaRef,
    batches: VecDeque<RecordBatch>,
    fetched: usize,
    rows: usize,
    open: bool,
}

fn failure(message: impl ToString) -> Error {
    Error::Other(message.to_string())
}

fn handle<T: 'static>(object: Robj) -> Result<ExternalPtr<T>> {
    // extendr's downcast dereferences the pointer. Check our tag first so an
    // unrelated Arrow/C pointer is never interpreted as extendr's Box<Any>.
    if !object.is_external_pointer() {
        return Err(failure("invalid ckdata native handle"));
    }
    // SAFETY: EXTPTRSXP was checked above. Inspect only its R tag, not its address.
    let tag = unsafe { object.external_ptr_tag() };
    if tag.as_str() != Some(std::any::type_name::<T>()) {
        return Err(failure("invalid ckdata native handle"));
    }
    ExternalPtr::try_from(object)
}

struct RuntimeState {
    runtime: tokio::runtime::Runtime,
    jobs: Vec<(Weak<QueryCancellation>, tokio::task::AbortHandle)>,
}

static RUNTIME: OnceLock<(u32, Mutex<Option<RuntimeState>>)> = OnceLock::new();

fn runtime_state() -> Result<&'static Mutex<Option<RuntimeState>>> {
    let (process_id, state) = RUNTIME.get_or_init(|| (std::process::id(), Mutex::new(None)));
    // A fork inherits the mutex and Tokio state, but none of their worker
    // threads. Reject it before locking or attempting to drop that state.
    if *process_id != std::process::id() {
        return Err(failure(
            "ckdata native runtime was inherited by a forked process; use a fresh R session or PSOCK workers",
        ));
    }
    Ok(state)
}

fn spawn(
    cancellation: QueryCancellation,
    future: impl Future<Output = NativeValue> + Send + 'static,
) -> Result<Robj> {
    let value = Arc::new(Mutex::new(None));
    let output = value.clone();
    let cancellation = Arc::new(cancellation);
    let task = {
        let mut state = runtime_state()?.lock().map_err(failure)?;
        if state.is_none() {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .map_err(failure)?;
            *state = Some(RuntimeState {
                runtime,
                jobs: Vec::new(),
            });
        }
        let state = state.as_mut().expect("runtime initialized above");
        state.jobs.retain(|(_, task)| !task.is_finished());
        let task = state.runtime.spawn(async move {
            let result = future.await;
            if let Ok(mut slot) = output.lock() {
                *slot = Some(result);
            }
        });
        state
            .jobs
            .push((Arc::downgrade(&cancellation), task.abort_handle()));
        task
    };
    Ok(ExternalPtr::new(NativeJob {
        value,
        cancellation,
        task,
    })
    .into())
}

fn connection(object: Robj) -> Result<Connection> {
    let native = handle::<NativeConnection>(object)?;
    let connection = native
        .connection
        .as_ref()
        .ok_or_else(|| failure("connection is closed"))?;
    connection.session_status().map_err(failure)?;
    Ok(connection.clone())
}

/// @noRd
#[extendr]
fn native_connect(
    workgroup: String,
    schema: String,
    profile: Option<String>,
    region: Option<String>,
    role_arn: Option<String>,
) -> Result<Robj> {
    let mut options = ConnectionOptions::new(workgroup, schema);
    options.profile = profile;
    options.region = region;
    options.role_arn = role_arn;
    let cancellation = QueryCancellation::new();
    let cancel = cancellation.clone();
    spawn(cancellation, async move {
        tokio::select! {
            _ = cancel.cancelled() => Err(NativeFailure::message("connection cancelled")),
            value = async {
                let connection = Connection::connect(options).await.map_err(NativeFailure::from)?;
                let identity = connection.caller_identity().await.map_err(NativeFailure::from)?;
                let options = connection.options().clone();
                let region = connection.region().to_owned();
                Ok(Completion::Connected(Box::new(NativeConnection { connection: Some(connection), options, region, identity })))
            } => value
        }
    })
}

/// @noRd
#[extendr]
fn native_ready(object: Robj) -> Result<bool> {
    let job = handle::<NativeJob>(object)?;
    Ok(job.value.lock().map_err(failure)?.is_some() || job.task.is_finished())
}

/// @noRd
#[extendr]
fn native_cancel(object: Robj) -> Result<()> {
    handle::<NativeJob>(object)?.cancellation.cancel();
    Ok(())
}

fn columns(table: ckdata::TableMetadata) -> List {
    let columns: Vec<_> = table
        .columns
        .into_iter()
        .chain(table.partition_keys)
        .collect();
    list!(
        name = columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
        data_type = columns
            .iter()
            .map(|c| c.data_type.clone().unwrap_or_default())
            .collect::<Vec<_>>()
    )
}

/// @noRd
#[extendr]
fn native_job_value(object: Robj) -> Result<List> {
    let job = handle::<NativeJob>(object)?;
    let value = job
        .value
        .lock()
        .map_err(failure)?
        .take()
        .ok_or_else(|| failure("job is not ready or was already consumed"))?;
    match value {
        Ok(value) => Ok(list!(success = true, value = completion_value(value)?)),
        Err(error) => Ok(list!(success = false, error = error.into_list())),
    }
}

fn completion_value(value: Completion) -> Result<Robj> {
    Ok(match value {
        Completion::Connected(value) => ExternalPtr::new(*value).into(),
        Completion::Query(value) => {
            let query_id = value.query_id().to_owned();
            let warnings: Vec<_> = value
                .warnings()
                .iter()
                .map(|warning| match warning {
                    ckdata::QueryWarning::SessionExpiring { remaining } => format!(
                        "assumed-role session expires in {} seconds; it will not renew",
                        remaining.as_secs()
                    ),
                })
                .collect();
            let result = NativeResult::from_reader(value.into_reader()).map_err(failure)?;
            list!(
                handle = ExternalPtr::new(result),
                query_id = query_id,
                warnings = warnings
            )
            .into()
        }
        Completion::Table(value) => columns(value).into(),
        Completion::Tables(value) => value.into(),
        Completion::Workgroup(value) => value.into_list().into(),
        Completion::Probe(value) => list!(
            handle = ExternalPtr::new(value),
            query_id = "synthetic",
            warnings = Vec::<String>::new()
        )
        .into(),
    })
}

/// @noRd
#[extendr]
fn native_connection_info(object: Robj) -> Result<List> {
    let native = handle::<NativeConnection>(object)?;
    let status = native.connection.as_ref().map(Connection::session_status);
    Ok(list!(
        open = native.connection.is_some(),
        account = native.identity.account.clone(),
        arn = native.identity.arn.clone(),
        region = native.region.clone(),
        workgroup = native.options.workgroup.clone(),
        schema = native.options.schema.clone(),
        session_error = match &status {
            Some(Err(error)) => error.to_string(),
            _ => String::new(),
        },
        expiring = matches!(status, Some(Ok(SessionStatus::Expiring { .. })))
    ))
}

/// @noRd
#[extendr]
fn native_disconnect(object: Robj) -> Result<()> {
    handle::<NativeConnection>(object)?.connection.take();
    Ok(())
}

/// Drain cancellation before the package's worker runtime is released. Called
/// from R's unload hook on the R thread, never from a Tokio worker.
/// @noRd
#[extendr]
fn native_shutdown() -> Result<()> {
    // Unloading without having started a worker must not claim runtime
    // ownership: a later fork may still initialize its own runtime.
    if RUNTIME.get().is_none() {
        return Ok(());
    }
    let state = runtime_state()?.lock().map_err(failure)?.take();
    if let Some(state) = state {
        for (cancellation, _) in &state.jobs {
            if let Some(cancellation) = cancellation.upgrade() {
                cancellation.cancel();
            }
        }
        state.runtime.block_on(async {
            while state.jobs.iter().any(|(_, job)| !job.is_finished()) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        // Drop joins the worker threads before the native library can unload.
        drop(state);
    }
    Ok(())
}

/// @noRd
#[extendr]
fn native_query(
    object: Robj,
    sql: String,
    timeout: f64,
    parameters: Vec<String>,
    schema_only: bool,
) -> Result<Robj> {
    let connection = connection(object)?;
    if !timeout.is_finite() || timeout <= 0.0 || timeout > 86400.0 {
        return Err(failure("timeout must be between zero and 86400 seconds"));
    }
    let options = QueryOptions {
        timeout: Duration::from_secs_f64(timeout),
        execution_parameters: parameters,
        ..QueryOptions::default()
    };
    let cancellation = QueryCancellation::new();
    let cancel = cancellation.clone();
    spawn(cancellation, async move {
        let value = if schema_only {
            connection.query_empty(&sql, &options, &cancel).await
        } else {
            connection.query(&sql, &options, &cancel).await
        };
        value.map(Completion::Query).map_err(NativeFailure::from)
    })
}

/// @noRd
#[extendr]
fn native_metadata(object: Robj, schema: String, table: Option<String>) -> Result<Robj> {
    let connection = connection(object)?.with_schema(&schema).map_err(failure)?;
    let cancellation = QueryCancellation::new();
    let cancel = cancellation.clone();
    spawn(cancellation, async move {
        tokio::select! {
            _ = cancel.cancelled() => Err(NativeFailure::message("metadata request cancelled")),
            value = async {
                if let Some(table) = table {
                    return connection.table_metadata(&table).await.map(Completion::Table).map_err(NativeFailure::from);
                }
                let mut tables = Vec::new();
                let mut token = None;
                loop {
                    let page = connection.table_page(token.as_deref()).await.map_err(NativeFailure::from)?;
                    tables.extend(page.tables.into_iter().map(|table| table.name));
                    token = page.next_token;
                    if token.is_none() { return Ok(Completion::Tables(tables)); }
                }
            } => value
        }
    })
}

/// Inspect workgroup metadata for acceptance preflight without submitting SQL.
/// @noRd
#[extendr]
fn native_workgroup(object: Robj) -> Result<Robj> {
    let connection = connection(object)?;
    let cancellation = QueryCancellation::new();
    let cancel = cancellation.clone();
    spawn(cancellation, async move {
        tokio::select! {
            _ = cancel.cancelled() => Err(NativeFailure::message("workgroup request cancelled")),
            value = connection.workgroup_configuration() => {
                let config = value.map_err(NativeFailure::from)?;
                let result = config.result_configuration();
                let encryption = result.and_then(|r| r.encryption_configuration());
                Ok(Completion::Workgroup(WorkgroupInfo {
                    output_location: result.and_then(|r| r.output_location()).map(str::to_owned),
                    expected_bucket_owner: result.and_then(|r| r.expected_bucket_owner()).map(str::to_owned),
                    encryption_option: encryption.map(|e| e.encryption_option().as_str().to_owned()),
                    kms_key: encryption.and_then(|e| e.kms_key()).map(str::to_owned),
                    enforce_work_group_configuration: config.enforce_work_group_configuration(),
                    bytes_scanned_cutoff_per_query: config.bytes_scanned_cutoff_per_query(),
                    selected_engine_version: config.engine_version().and_then(|e| e.selected_engine_version()).map(str::to_owned),
                }))
            }
        }
    })
}

impl NativeResult {
    fn from_reader(
        reader: impl RecordBatchReader,
    ) -> std::result::Result<Self, arrow_schema::ArrowError> {
        let schema = reader.schema();
        let batches: VecDeque<_> = reader.collect::<std::result::Result<_, _>>()?;
        let rows = batches.iter().map(RecordBatch::num_rows).sum();
        Ok(Self {
            schema,
            batches,
            rows,
            fetched: 0,
            open: true,
        })
    }

    fn fetch(&mut self, n: usize) -> Vec<RecordBatch> {
        let mut remaining = n.min(self.rows - self.fetched);
        let mut result = Vec::new();
        while remaining > 0 {
            let Some(batch) = self.batches.pop_front() else {
                break;
            };
            let take = remaining.min(batch.num_rows());
            if take > 0 {
                result.push(batch.slice(0, take));
            }
            if take < batch.num_rows() {
                self.batches
                    .push_front(batch.slice(take, batch.num_rows() - take));
            }
            remaining -= take;
            self.fetched += take;
        }
        result
    }
}

/// @noRd
#[extendr]
fn native_result_fetch(object: Robj, n: f64) -> Result<Robj> {
    let mut result = handle::<NativeResult>(object)?;
    if !result.open {
        return Err(failure("result is closed"));
    }
    let count = if n == -1.0 || n == f64::INFINITY || n.is_nan() {
        result.rows - result.fetched
    } else if n.is_finite() && n >= 0.0 && n.fract() == 0.0 {
        n.min(usize::MAX as f64) as usize
    } else {
        return Err(failure(
            "n must be a non-negative whole number, -1, NA or Inf",
        ));
    };
    let schema = result.schema.clone();
    let batches = result.fetch(count);
    crate::owned_stream(RecordBatchIterator::new(
        batches.into_iter().map(Ok),
        schema,
    ))
}

/// @noRd
#[extendr]
fn native_result_info(object: Robj) -> Result<List> {
    let result = handle::<NativeResult>(object)?;
    Ok(list!(
        open = result.open,
        fetched = result.fetched as f64,
        rows = result.rows as f64,
        name = result
            .schema
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect::<Vec<_>>(),
        data_type = result
            .schema
            .fields()
            .iter()
            .map(|f| f.data_type().to_string())
            .collect::<Vec<_>>()
    ))
}

/// Concatenate vectorized parameter results without converting Arrow values.
/// @noRd
#[extendr]
fn native_result_append(object: Robj, other: Robj) -> Result<()> {
    if object == other {
        return Err(failure("cannot append a result to itself"));
    }
    let mut result = handle::<NativeResult>(object)?;
    let mut other = handle::<NativeResult>(other)?;
    if !result.open || !other.open || result.fetched != 0 || other.fetched != 0 {
        return Err(failure("cannot append closed or partially fetched results"));
    }
    if result.schema != other.schema {
        return Err(failure("parameter sets returned different result schemas"));
    }
    result.rows += other.rows;
    result.batches.append(&mut other.batches);
    other.rows = 0;
    other.open = false;
    Ok(())
}

/// @noRd
#[extendr]
fn native_result_clear(object: Robj) -> Result<()> {
    let mut result = handle::<NativeResult>(object)?;
    result.batches.clear();
    result.open = false;
    Ok(())
}

/// Native result fixture. Entirely offline, with the same cursor as AWS results.
/// @noRd
#[extendr]
fn native_probe_result(empty: bool) -> Result<Robj> {
    let mut result =
        NativeResult::from_reader(ckdata::arrow_probe().map_err(failure)?).map_err(failure)?;
    if empty {
        result.batches.clear();
        result.rows = 0;
    }
    Ok(ExternalPtr::new(result).into())
}

/// Offline workgroup fixture using the same R conversion as live metadata.
/// @noRd
#[extendr]
fn native_probe_workgroup() -> List {
    WorkgroupInfo {
        output_location: Some(
            "s3://ck-datalake-tst-ckdata-fixtures-000000000000/results/internal/".to_owned(),
        ),
        expected_bucket_owner: Some("000000000000".to_owned()),
        encryption_option: Some("SSE_S3".to_owned()),
        kms_key: None,
        enforce_work_group_configuration: Some(true),
        bytes_scanned_cutoff_per_query: Some(10 * 1024 * 1024),
        selected_engine_version: Some("Athena engine version 3".to_owned()),
    }
    .into_list()
}

/// Offline table fixture, including a partition key, through live metadata conversion.
/// @noRd
#[extendr]
fn native_probe_metadata() -> List {
    let column = |name: &str, data_type: &str| ckdata::ColumnMetadata {
        name: name.to_owned(),
        data_type: Some(data_type.to_owned()),
        comment: None,
    };
    columns(ckdata::TableMetadata {
        name: "types".to_owned(),
        table_type: None,
        columns: vec![
            column("id", "integer"),
            column("s_null", "varchar"),
            column("s_empty", "varchar"),
            column("big", "bigint"),
        ],
        partition_keys: vec![column("ts", "timestamp")],
    })
}

/// Offline asynchronous fixture for interrupt and task-lifetime tests.
/// @noRd
#[extendr]
fn native_probe_job(milliseconds: f64) -> Result<Robj> {
    if !milliseconds.is_finite() || !(0.0..=60000.0).contains(&milliseconds) {
        return Err(failure("invalid fixture delay"));
    }
    let cancellation = QueryCancellation::new();
    let cancel = cancellation.clone();
    spawn(cancellation, async move {
        tokio::select! {
            _ = cancel.cancelled() => Err(NativeFailure::message("synthetic query cancelled")),
            _ = tokio::time::sleep(Duration::from_secs_f64(milliseconds / 1000.0)) => {
                NativeResult::from_reader(ckdata::arrow_probe().map_err(NativeFailure::message)?)
                    .map(Completion::Probe).map_err(NativeFailure::message)
            }
        }
    })
}

extendr_module! {
    mod bridge;
    fn native_connect;
    fn native_ready;
    fn native_cancel;
    fn native_job_value;
    fn native_connection_info;
    fn native_disconnect;
    fn native_shutdown;
    fn native_query;
    fn native_metadata;
    fn native_workgroup;
    fn native_result_fetch;
    fn native_result_info;
    fn native_result_clear;
    fn native_result_append;
    fn native_probe_result;
    fn native_probe_workgroup;
    fn native_probe_metadata;
    fn native_probe_job;
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::{Array, Int64Array};

    #[test]
    fn native_failures_preserve_core_cancellation_metadata() {
        let error = ckdata::Error::SchemaRecovery {
            query_id: "original".into(),
            source: Box::new(ckdata::Error::QueryInterrupted {
                query_id: Some("recovery".into()),
                reason: ckdata::QueryInterruption::Cancelled,
                cancellation: ckdata::CancellationStatus::Unconfirmed,
            }),
        };
        let failure = NativeFailure::from(error);
        assert_eq!(
            failure.cancellation,
            Some(ckdata::CancellationStatus::Unconfirmed)
        );
        assert!(failure.remote_may_be_running);
        let local = NativeFailure::message("unconfirmed is only text");
        assert_eq!(local.cancellation, None);
        assert!(!local.remote_may_be_running);
    }

    #[test]
    fn cursor_fetches_across_empty_batches_without_losing_rows_or_completion() {
        let batch = RecordBatch::try_from_iter(vec![(
            "id",
            Arc::new(Int64Array::from(vec![
                Some(1),
                Some(2),
                None,
                Some(9_007_199_254_740_993),
            ])) as arrow_array::ArrayRef,
        )])
        .unwrap();
        let schema = batch.schema();
        let batches = vec![
            batch.slice(0, 0),
            batch.slice(0, 2),
            batch.slice(0, 0),
            batch.slice(0, 0),
            batch.slice(2, 2),
            batch.slice(0, 0),
        ];
        for chunk_size in [1, 2, 3, 10] {
            let reader =
                RecordBatchIterator::new(batches.clone().into_iter().map(Ok), schema.clone());
            let mut result = NativeResult::from_reader(reader).unwrap();
            assert!(result.fetch(0).is_empty());
            assert_eq!(result.fetched, 0);
            let mut values = Vec::new();
            let max_fetches = result.rows;
            for _ in 0..max_fetches {
                if result.fetched == result.rows {
                    break;
                }
                let before = result.fetched;
                let batches = result.fetch(chunk_size);
                let returned = batches.iter().map(RecordBatch::num_rows).sum::<usize>();
                assert!(
                    returned > 0,
                    "each fetch must progress through empty batches"
                );
                assert_eq!(
                    returned,
                    chunk_size.min(result.rows - before),
                    "fetch must fill its row quota across batches"
                );
                assert!(result.fetched <= result.rows);
                assert_eq!(result.fetched - before, returned);
                for batch in batches {
                    values.extend(
                        batch
                            .column(0)
                            .as_any()
                            .downcast_ref::<Int64Array>()
                            .unwrap()
                            .iter(),
                    );
                }
            }
            assert_eq!(
                values,
                vec![Some(1), Some(2), None, Some(9_007_199_254_740_993)]
            );
            assert_eq!(result.fetched, 4);
            assert_eq!(result.schema, schema);
            assert!(result.fetch(chunk_size).is_empty());
            assert_eq!(result.fetched, 4);
        }
    }
}
