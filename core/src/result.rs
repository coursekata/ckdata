use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use arrow_array::{RecordBatch, RecordBatchIterator, RecordBatchReader};
use arrow_schema::SchemaRef;
use futures_util::{StreamExt, TryStreamExt, stream};
use tokio::time::Instant;
use uuid::Uuid;

use crate::query::{RequestGuard, read_query};
use crate::{Connection, Error, QueryCancellation, QueryInterruption, QueryOptions};

mod decode;
mod transport;

/// Collection limits, rather than a promise about total process memory. Sorting
/// temporarily needs concatenated and sorted copies plus row indices. Parquet
/// decoding also needs scratch space. Increase limits deliberately for big reads.
#[derive(Clone, Debug)]
pub struct ResultLimits {
    pub max_manifest_bytes: usize,
    pub max_files: usize,
    pub max_file_bytes: usize,
    pub max_download_bytes: usize,
    pub max_decoded_bytes: usize,
    pub max_rows: usize,
    pub batch_size: usize,
    pub download_concurrency: usize,
}

impl Default for ResultLimits {
    fn default() -> Self {
        Self {
            max_manifest_bytes: 1024 * 1024,
            max_files: 256,
            max_file_bytes: 64 * 1024 * 1024,
            max_download_bytes: 256 * 1024 * 1024,
            max_decoded_bytes: 256 * 1024 * 1024,
            max_rows: 1_000_000,
            batch_size: 8192,
            download_concurrency: 4,
        }
    }
}

impl ResultLimits {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if [
            self.max_manifest_bytes,
            self.max_files,
            self.max_file_bytes,
            self.max_download_bytes,
            self.max_decoded_bytes,
            self.max_rows,
            self.batch_size,
            self.download_concurrency,
        ]
        .contains(&0)
            || self.max_rows > u32::MAX as usize - 1
            || self.batch_size > 65536
            || self.download_concurrency > 16
        {
            return Err(Error::InvalidSetting("result limits"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryWarning {
    /// The result rows are correct, but this outer ORDER BY cannot be reproduced
    /// from returned columns with supported local sort semantics.
    OrderNotRestored { reason: &'static str },
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ResultError {
    #[error("result exceeds the configured {0} limit")]
    LimitExceeded(&'static str),
    #[error("invalid manifest: {0}")]
    InvalidManifest(&'static str),
    #[error("invalid Parquet result: {0}")]
    InvalidParquet(&'static str),
    #[error("invalid result schema: {0}")]
    InvalidSchema(&'static str),
    #[error("unsupported result type: {0}")]
    UnsupportedType(&'static str),
    #[error("object encryption differs from the workgroup configuration")]
    EncryptionMismatch,
    #[error("{0}")]
    Interrupted(QueryInterruption),
}

/// An owned, bounded collection of Arrow batches. Values remain in Arrow rather
/// than passing through text, JSON or floating-point conversion.
pub struct QueryResult {
    query_id: String,
    schema: SchemaRef,
    batches: Vec<RecordBatch>,
    warnings: Vec<QueryWarning>,
}

impl std::fmt::Debug for QueryResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QueryResult")
            .field("query_id", &self.query_id)
            .field("schema", &self.schema)
            .field("row_count", &self.row_count())
            .field("warnings", &self.warnings)
            .finish_non_exhaustive()
    }
}

impl QueryResult {
    pub fn query_id(&self) -> &str {
        &self.query_id
    }
    pub fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }
    pub fn batches(&self) -> &[RecordBatch] {
        &self.batches
    }
    pub fn warnings(&self) -> &[QueryWarning] {
        &self.warnings
    }
    pub fn row_count(&self) -> usize {
        self.batches.iter().map(RecordBatch::num_rows).sum()
    }
    pub fn into_reader(self) -> impl RecordBatchReader + Send {
        RecordBatchIterator::new(self.batches.into_iter().map(Ok), self.schema)
    }
}

impl Connection {
    /// Read one SELECT/VALUES query into Arrow, within the supplied limits.
    /// A private presence column makes even an empty result carry its Parquet
    /// schema. It and the single synthetic empty row never reach the caller.
    /// Keep awaiting this future after signalling cancellation.
    pub async fn query(
        &self,
        sql: &str,
        options: &QueryOptions,
        cancellation: &QueryCancellation,
    ) -> Result<QueryResult, Error> {
        options.validate()?;
        let deadline = Instant::now() + options.timeout;
        let query = read_query(sql)?;
        let marker = format!("__ckdata_presence_{}", Uuid::new_v4().simple());
        let wrapped = presence_query(&query.to_string(), &marker);
        let execution = self
            .execute_sql_unload(wrapped, options, cancellation, deadline)
            .await?;
        let result_cancellation = cancellation.child_token();
        let _cancel_background_work = result_cancellation.clone().drop_guard();
        let guard = RequestGuard {
            connection: self,
            cancellation: &result_cancellation,
            deadline,
        };
        let budget = Arc::new(Budget::default());
        let files = transport::manifest(self, &execution, &options.result_limits, &guard)
            .await
            .map_err(|reason| result_error(&execution.query_id, reason))?;
        let mut pending = stream::iter(files.into_iter().enumerate())
            .map(|(index, file)| {
                let budget = budget.clone();
                let marker = marker.clone();
                let execution = &execution;
                let guard = &guard;
                let cancellation = result_cancellation.clone();
                async move {
                    let bytes = transport::download(
                        self,
                        &file,
                        execution,
                        &options.result_limits,
                        guard,
                        &budget,
                    )
                    .await?;
                    let connection = self.clone();
                    let limits = options.result_limits.clone();
                    let decoded = tokio::task::spawn_blocking(move || {
                        decode::decode(
                            bytes,
                            &marker,
                            &limits,
                            &budget,
                            &RequestGuard {
                                connection: &connection,
                                cancellation: &cancellation,
                                deadline,
                            },
                        )
                    })
                    .await
                    .map_err(|_| ResultError::InvalidParquet("decoder task failed"))??;
                    Ok::<_, ResultError>((index, decoded))
                }
            })
            .buffer_unordered(options.result_limits.download_concurrency);
        let mut decoded = Vec::new();
        while let Some(item) = pending
            .try_next()
            .await
            .map_err(|reason| result_error(&execution.query_id, reason))?
        {
            decoded.push(item);
        }
        drop(pending);
        decoded.sort_by_key(|(index, _)| *index);
        let schema = decoded
            .first()
            .ok_or_else(|| {
                result_error(
                    &execution.query_id,
                    ResultError::InvalidManifest("no schema-bearing files"),
                )
            })?
            .1
            .schema
            .clone();
        let mut batches = Vec::new();
        let mut sentinels = 0;
        for (_, file) in decoded {
            if file.schema != schema {
                return Err(result_error(
                    &execution.query_id,
                    ResultError::InvalidSchema("files have inconsistent schemas"),
                ));
            }
            sentinels += file.sentinels;
            batches.extend(file.batches);
        }
        let rows: usize = batches.iter().map(RecordBatch::num_rows).sum();
        if rows > options.result_limits.max_rows {
            return Err(result_error(
                &execution.query_id,
                ResultError::LimitExceeded("row count"),
            ));
        }
        if sentinels > 1 || (sentinels != 0 && rows != 0) {
            return Err(result_error(
                &execution.query_id,
                ResultError::InvalidParquet("inconsistent empty-result marker"),
            ));
        }
        guard.check().map_err(|reason| {
            result_error(&execution.query_id, ResultError::Interrupted(reason))
        })?;
        let plan = crate::ordering::plan(&query, &schema);
        let (batches, warnings) = match plan {
            Ok(keys) if !keys.is_empty() && rows > 1 => {
                let connection = self.clone();
                let cancellation = result_cancellation.clone();
                let sorted = tokio::task::spawn_blocking(move || {
                    let guard = RequestGuard {
                        connection: &connection,
                        cancellation: &cancellation,
                        deadline,
                    };
                    guard.check().map_err(ResultError::Interrupted)?;
                    let batches = crate::ordering::sort(batches, &keys)?;
                    guard.check().map_err(ResultError::Interrupted)?;
                    Ok::<_, ResultError>(batches)
                })
                .await
                .map_err(|_| {
                    result_error(
                        &execution.query_id,
                        ResultError::InvalidParquet("sort task failed"),
                    )
                })?
                .map_err(|reason| result_error(&execution.query_id, reason))?;
                (sorted, vec![])
            }
            Ok(_) => (batches, vec![]),
            Err(warning) => (batches, vec![warning]),
        };
        Ok(QueryResult {
            query_id: execution.query_id,
            schema,
            batches,
            warnings,
        })
    }
}

fn presence_query(query: &str, marker: &str) -> String {
    format!(
        "SELECT ckdata_rows.* FROM (VALUES (TRUE)) AS ckdata_seed(unused) LEFT JOIN (SELECT ckdata_input.*, TRUE AS \"{marker}\" FROM ({query}) AS ckdata_input) AS ckdata_rows ON TRUE"
    )
}

fn result_error(query_id: &str, reason: ResultError) -> Error {
    Error::Result {
        query_id: query_id.to_owned(),
        reason,
    }
}

#[derive(Default)]
struct Budget {
    downloaded: AtomicUsize,
    uncompressed: AtomicUsize,
    decoded: AtomicUsize,
    rows: AtomicUsize,
}

fn reserve(
    counter: &AtomicUsize,
    bytes: usize,
    limit: usize,
    name: &'static str,
) -> Result<(), ResultError> {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let next = current
            .checked_add(bytes)
            .filter(|total| *total <= limit)
            .ok_or(ResultError::LimitExceeded(name))?;
        match counter.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Ok(()),
            Err(updated) => current = updated,
        }
    }
}

#[cfg(test)]
mod tests;
