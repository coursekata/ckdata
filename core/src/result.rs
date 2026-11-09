use arrow_array::{RecordBatch, RecordBatchIterator, RecordBatchReader};
use arrow_schema::SchemaRef;
use futures_util::{StreamExt, TryStreamExt, stream};
use tokio::time::Instant;

use crate::query::RequestGuard;
use crate::{Connection, Error, QueryCancellation, QueryInterruption, QueryOptions};

mod decode;
pub(crate) mod transport;

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
/// Empty results retain their Parquet schema. This collection imposes no row
/// or byte limit.
pub struct QueryResult {
    query_id: String,
    schema: SchemaRef,
    batches: Vec<RecordBatch>,
}

impl std::fmt::Debug for QueryResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QueryResult")
            .field("query_id", &self.query_id)
            .field("schema", &self.schema)
            .field("row_count", &self.row_count())
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
    pub fn row_count(&self) -> usize {
        self.batches.iter().map(RecordBatch::num_rows).sum()
    }
    /// Transfer the Arrow data and schema.
    pub fn into_reader(self) -> impl RecordBatchReader + Send {
        RecordBatchIterator::new(self.batches.into_iter().map(Ok), self.schema)
    }
}

impl Connection {
    /// Run the supplied SQL through Parquet UNLOAD and decode its result files
    /// with the Parquet library. A fileless empty result needs one schema-only
    /// UNLOAD; its placeholder rows are never returned. Both executions share
    /// the timeout and cancellation signal. There is no global row-order guarantee.
    /// Keep awaiting this future after signalling cancellation.
    pub async fn query(
        &self,
        sql: &str,
        options: &QueryOptions,
        cancellation: &QueryCancellation,
    ) -> Result<QueryResult, Error> {
        options.validate()?;
        let deadline = Instant::now() + options.timeout;
        let sql = crate::query::query_text(sql)?;
        let execution = self
            .execute_sql_unload(sql.to_owned(), options, cancellation, deadline)
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
        let mut schema = if files.is_empty() {
            Some(
                self.recover_empty_schema(sql, options, &guard)
                    .await
                    .map_err(|source| Error::SchemaRecovery {
                        query_id: execution.query_id.clone(),
                        source: Box::new(source),
                    })?
                    .1,
            )
        } else {
            None
        };
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
        let schema = schema.ok_or_else(|| {
            result_error(
                &execution.query_id,
                ResultError::InvalidSchema("result has no Parquet schema"),
            )
        })?;
        Ok(QueryResult {
            query_id: execution.query_id,
            schema,
            batches,
        })
    }

    /// Obtain a typed empty result without executing the data-producing query.
    /// Used for zero-length DBI parameter sets. This uses the same Parquet
    /// schema recovery as fileless query results, with the supplied parameters.
    pub async fn query_empty(
        &self,
        sql: &str,
        options: &QueryOptions,
        cancellation: &QueryCancellation,
    ) -> Result<QueryResult, Error> {
        options.validate()?;
        let guard = RequestGuard {
            connection: self,
            cancellation,
            deadline: Instant::now() + options.timeout,
        };
        let (query_id, schema) = self
            .recover_empty_schema(crate::query::query_text(sql)?, options, &guard)
            .await?;
        Ok(QueryResult {
            query_id,
            schema,
            batches: Vec::new(),
        })
    }

    async fn recover_empty_schema(
        &self,
        sql: &str,
        options: &QueryOptions,
        guard: &RequestGuard<'_>,
    ) -> Result<(String, SchemaRef), Error> {
        // LIMIT belongs outside the complete original query. The singleton
        // join supplies typed NULLs even when Athena otherwise writes no files.
        // Only this Parquet schema is used; no recovery rows are decoded.
        let query = format!(
            "SELECT schema_rows.*\nFROM (VALUES (1)) AS seed(unused)\nLEFT JOIN (SELECT * FROM (\n{sql}\n) AS original_rows LIMIT 0) AS schema_rows ON TRUE"
        );
        let execution = self
            .execute_sql_unload(query, options, guard.cancellation, guard.deadline)
            .await?;
        let files = transport::manifest(self, &execution, guard)
            .await
            .map_err(|reason| result_error(&execution.query_id, reason))?;
        let mut schema = None;
        for file in files {
            let bytes = transport::download(self, &file, &execution, guard)
                .await
                .map_err(|reason| result_error(&execution.query_id, reason))?;
            let connection = self.clone();
            let cancellation = guard.cancellation.clone();
            let deadline = guard.deadline;
            let task = tokio::task::spawn_blocking(move || {
                decode::schema(
                    bytes,
                    &RequestGuard {
                        connection: &connection,
                        cancellation: &cancellation,
                        deadline,
                    },
                )
            });
            let recovered = wait_for_worker(guard, task)
                .await
                .map_err(|reason| result_error(&execution.query_id, reason))?;
            if let Some(expected) = &schema {
                if expected != &recovered {
                    return Err(result_error(
                        &execution.query_id,
                        ResultError::InvalidSchema("recovery files have inconsistent schemas"),
                    ));
                }
            } else {
                schema = Some(recovered);
            }
        }
        schema
            .map(|schema| (execution.query_id.clone(), schema))
            .ok_or_else(|| {
                result_error(
                    &execution.query_id,
                    ResultError::InvalidSchema("empty-result recovery produced no Parquet schema"),
                )
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
