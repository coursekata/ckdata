use arrow_array::{RecordBatch, RecordBatchIterator, RecordBatchReader};
use arrow_schema::SchemaRef;
use futures_util::{StreamExt, TryStreamExt, stream};
use tokio::time::Instant;

use crate::query::RequestGuard;
use crate::{Connection, Error, QueryCancellation, QueryInterruption, QueryOptions};

mod decode;
pub(crate) mod transport;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryWarning {
    /// Hosts should display this before presenting the next result.
    SessionExpiring { remaining: std::time::Duration },
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ResultError {
    #[error("invalid manifest: {0}")]
    InvalidManifest(&'static str),
    #[error("invalid Parquet result: {0}")]
    InvalidParquet(&'static str),
    #[error("invalid result schema: {0}")]
    InvalidSchema(&'static str),
    #[error("object encryption differs from the workgroup configuration")]
    EncryptionMismatch,
    #[error("{0}")]
    Interrupted(QueryInterruption),
}

/// Arrow batches decoded from Athena's result files, without local row sorting.
/// A result with no files has no schema; an actual empty Parquet file retains
/// its schema. This collection imposes no row or byte limit.
pub struct QueryResult {
    query_id: String,
    schema: Option<SchemaRef>,
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
    /// None when Athena produced no schema-bearing Parquet file.
    pub fn schema(&self) -> Option<SchemaRef> {
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
    /// Transfer the Arrow data when a schema exists. Read warnings first, or
    /// use into_reader_with_warnings to retain them alongside an absent reader.
    pub fn into_reader(self) -> Option<impl RecordBatchReader + Send> {
        self.schema
            .map(|schema| RecordBatchIterator::new(self.batches.into_iter().map(Ok), schema))
    }
    pub fn into_reader_with_warnings(
        self,
    ) -> (Option<impl RecordBatchReader + Send>, Vec<QueryWarning>) {
        let warnings = self.warnings;
        let reader = self
            .schema
            .map(|schema| RecordBatchIterator::new(self.batches.into_iter().map(Ok), schema));
        (reader, warnings)
    }
}

impl Connection {
    /// Run the supplied SQL through Parquet UNLOAD and decode its result files
    /// with the Parquet library. SQL is not rewritten to manufacture empty rows,
    /// and UNLOAD's lack of global file ordering is preserved.
    /// Keep awaiting this future after signalling cancellation.
    pub async fn query(
        &self,
        sql: &str,
        options: &QueryOptions,
        cancellation: &QueryCancellation,
    ) -> Result<QueryResult, Error> {
        options.validate()?;
        let deadline = Instant::now() + options.timeout;
        let execution = self
            .execute_sql_unload(
                crate::query::query_text(sql)?.to_owned(),
                options,
                cancellation,
                deadline,
            )
            .await?;
        let result_cancellation = cancellation.child_token();
        let _cancel_background_work = result_cancellation.clone().drop_guard();
        let guard = RequestGuard {
            connection: self,
            cancellation: &result_cancellation,
            deadline,
        };
        let files = transport::manifest(self, &execution, &guard)
            .await
            .map_err(|reason| result_error(&execution.query_id, reason))?;
        let mut pending = stream::iter(files)
            .map(|file| {
                let execution = &execution;
                let guard = &guard;
                let cancellation = result_cancellation.clone();
                async move {
                    let bytes = transport::download(self, &file, execution, guard).await?;
                    let connection = self.clone();
                    let task = tokio::task::spawn_blocking(move || {
                        decode::decode(
                            bytes,
                            &RequestGuard {
                                connection: &connection,
                                cancellation: &cancellation,
                                deadline,
                            },
                        )
                    });
                    wait_for_worker(guard, task).await
                }
            })
            // A small transfer queue controls concurrency, not result size.
            .buffered(4);
        let mut schema = None;
        let mut batches = Vec::new();
        while let Some(file) = pending
            .try_next()
            .await
            .map_err(|reason| result_error(&execution.query_id, reason))?
        {
            if let Some(expected) = &schema {
                if expected != &file.schema {
                    return Err(result_error(
                        &execution.query_id,
                        ResultError::InvalidSchema("files have inconsistent schemas"),
                    ));
                }
            } else {
                schema = Some(file.schema);
            }
            batches.extend(file.batches);
        }
        drop(pending);
        guard.check().map_err(|reason| {
            result_error(&execution.query_id, ResultError::Interrupted(reason))
        })?;
        let mut warnings = Vec::new();
        if let crate::SessionStatus::Expiring { remaining, .. } =
            self.session_status().map_err(|_| {
                result_error(
                    &execution.query_id,
                    ResultError::Interrupted(QueryInterruption::SessionExpired),
                )
            })?
        {
            warnings.push(QueryWarning::SessionExpiring { remaining });
        }
        Ok(QueryResult {
            query_id: execution.query_id,
            schema,
            batches,
            warnings,
        })
    }
}

fn result_error(query_id: &str, reason: ResultError) -> Error {
    Error::Result {
        query_id: query_id.to_owned(),
        reason,
    }
}

async fn wait_for_worker<T>(
    guard: &RequestGuard<'_>,
    task: tokio::task::JoinHandle<Result<T, ResultError>>,
) -> Result<T, ResultError> {
    guard
        .run(async { Ok(task.await) })
        .await
        .map_err(ResultError::Interrupted)?
        .map_err(|_| ResultError::InvalidParquet("decoder task failed"))?
}

#[cfg(test)]
mod tests;
