use std::future::Future;
use std::time::Duration;

use aws_sdk_athena::Client;
use aws_sdk_athena::config::timeout::TimeoutConfig;
use aws_sdk_athena::error::ProvideErrorMetadata;
use aws_sdk_athena::types::{
    EncryptionOption, QueryExecutionContext, QueryExecutionState, ResultConfiguration,
    WorkGroupState,
};
use tokio::time::{Instant, sleep, sleep_until, timeout};
use uuid::Uuid;

use crate::{Connection, Error, QueryCancellation, SessionStatus};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// Limits apply to the query lifecycle. A stop request gets up to five additional
/// seconds. Each SDK call is capped at ten seconds, including submission.
#[derive(Clone, Debug)]
pub struct QueryOptions {
    pub timeout: Duration,
    pub poll_interval: Duration,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30 * 60),
            poll_interval: Duration::from_secs(1),
        }
    }
}

impl QueryOptions {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.timeout.is_zero()
            || self.timeout > Duration::from_secs(86400)
            || self.poll_interval.is_zero()
            || self.poll_interval > Duration::from_secs(60)
        {
            return Err(Error::InvalidSetting("query timeout or poll interval"));
        }
        Ok(())
    }
}

/// Why the client stopped waiting. This retains the original failure even when
/// Athena cannot be reached to stop the remote execution.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum QueryInterruption {
    #[error("cancelled")]
    Cancelled,
    #[error("deadline exceeded")]
    TimedOut,
    #[error("assumed-role session expired")]
    SessionExpired,
    #[error("AWS {operation} failed ({code}){detail}", detail = crate::error::message_suffix(.message))]
    RequestFailed {
        operation: &'static str,
        code: String,
        message: Option<String>,
    },
    #[error("AWS returned an invalid {0}")]
    InvalidResponse(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CancellationStatus {
    #[error("not submitted")]
    NotSubmitted,
    /// Athena accepted StopQueryExecution, or already reported CANCELLED.
    #[error("requested")]
    Requested,
    /// Credentials expired, the stop request failed, or its deadline elapsed.
    #[error("unconfirmed; the remote query may still be running")]
    Unconfirmed,
}

/// A completed UNLOAD, before downloading or decoding any result rows.
/// This low-level operation does not promise global row ordering across files.
#[derive(Clone, Debug)]
pub struct QueryExecution {
    pub query_id: String,
    pub manifest_location: Option<String>,
    pub data_prefix: String,
    pub result_configuration: ResultConfiguration,
}

impl Connection {
    /// Submit the supplied SQL inside Parquet UNLOAD and await its completion.
    /// Athena validates the enclosed query.
    /// Output goes under a fresh prefix in the workgroup's configured result
    /// location. No bucket listing or deletion is performed.
    ///
    /// Keep awaiting this future after signalling cancellation. During submission
    /// the client first waits for the query ID (at most ten seconds), then requests
    /// a stop. Dropping the future cannot guarantee remote cancellation.
    pub async fn execute_unload(
        &self,
        sql: &str,
        options: &QueryOptions,
        cancellation: &QueryCancellation,
    ) -> Result<QueryExecution, Error> {
        let query = query_text(sql)?;
        options.validate()?;
        self.execute_sql_unload(
            query.to_owned(),
            options,
            cancellation,
            Instant::now() + options.timeout,
        )
        .await
    }

    pub(crate) async fn execute_sql_unload(
        &self,
        query: String,
        options: &QueryOptions,
        cancellation: &QueryCancellation,
        deadline: Instant,
    ) -> Result<QueryExecution, Error> {
        options.validate()?;
        let guard = RequestGuard {
            connection: self,
            cancellation,
            deadline,
        };
        let client = Client::from_conf(
            aws_sdk_athena::config::Builder::from(self.aws_config())
                .timeout_config(
                    TimeoutConfig::builder()
                        .operation_timeout(REQUEST_TIMEOUT)
                        .build(),
                )
                .build(),
        );
        let workgroup = guard
            .run(async {
                client
                    .get_work_group()
                    .work_group(&self.options().workgroup)
                    .send()
                    .await
                    .map_err(|error| QueryInterruption::RequestFailed {
                        operation: "GetWorkGroup",
                        code: error
                            .as_service_error()
                            .and_then(|error| error.code())
                            .unwrap_or("request failed")
                            .to_owned(),
                        message: crate::error::service_message(error.as_service_error()),
                    })
            })
            .await
            .map_err(before_submission)?;
        let workgroup = workgroup
            .work_group()
            .ok_or(Error::InvalidServiceResponse("GetWorkGroup"))?;
        if workgroup.state() != Some(&WorkGroupState::Enabled) {
            return Err(Error::InvalidSetting("workgroup must be enabled"));
        }
        let configuration = workgroup
            .configuration()
            .and_then(|config| config.result_configuration())
            .ok_or(Error::InvalidSetting("workgroup result configuration"))?;
        let output = configuration
            .output_location()
            .ok_or(Error::InvalidSetting("workgroup output location"))?;
        let output = s3_prefix(output).ok_or(Error::InvalidSetting("workgroup output location"))?;
        if configuration
            .encryption_configuration()
            .is_some_and(|encryption| {
                !matches!(
                    encryption.encryption_option(),
                    EncryptionOption::SseS3 | EncryptionOption::SseKms
                )
            })
        {
            return Err(Error::InvalidSetting(
                "workgroup encryption must use server-side encryption",
            ));
        }
        if configuration
            .encryption_configuration()
            .is_some_and(|encryption| {
                encryption.encryption_option() == &EncryptionOption::SseKms
                    && encryption
                        .kms_key()
                        .is_none_or(|key| !crate::result::transport::valid_kms_key(key))
            })
        {
            return Err(Error::InvalidSetting(
                "workgroup KMS key must be a key ARN or UUID",
            ));
        }
        let request_token = Uuid::new_v4().to_string();
        let data_prefix = format!("{output}ckdata/{request_token}/");
        let destination = QueryDestination {
            data_prefix,
            output_prefix: output,
            configuration: configuration.clone(),
        };
        let unload = format!(
            "UNLOAD (\n{query}\n) TO '{}' WITH (format = 'PARQUET', compression = 'SNAPPY')",
            destination.data_prefix.replace('\'', "''")
        );
        guard.check().map_err(before_submission)?;
        // Do not race cancellation against StartQueryExecution: its response is
        // needed to stop a query accepted just before the interruption. The SDK
        // bounds this call and retries with the same idempotency token.
        let started = client
            .start_query_execution()
            .client_request_token(&request_token)
            .query_string(unload)
            .work_group(&self.options().workgroup)
            .query_execution_context(
                QueryExecutionContext::builder()
                    .catalog("AwsDataCatalog")
                    .database(&self.options().schema)
                    .build(),
            )
            .result_configuration(destination.configuration.clone())
            .send()
            .await
            .map_err(|error| Error::QuerySubmission {
                code: error
                    .as_service_error()
                    .and_then(|error| error.code())
                    .unwrap_or("request failed; submission outcome unknown")
                    .to_owned(),
                message: crate::error::service_message(error.as_service_error()),
                request_token: request_token.clone(),
            })?;
        let query_id = started
            .query_execution_id()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| Error::QuerySubmission {
                code: "missing query ID; submission outcome unknown".to_owned(),
                message: None,
                request_token,
            })?
            .to_owned();
        let result = self
            .poll_query(&client, &query_id, &destination, &guard, options)
            .await;
        match result {
            Ok(result) => Ok(result),
            Err(PollError::Failed(reason)) => Err(Error::QueryFailed { query_id, reason }),
            Err(PollError::Cancelled) => Err(Error::QueryInterrupted {
                query_id: Some(query_id),
                reason: QueryInterruption::Cancelled,
                cancellation: CancellationStatus::Requested,
            }),
            Err(PollError::Interrupted(reason)) => {
                let cancellation = self.stop_query(&client, &query_id).await;
                Err(Error::QueryInterrupted {
                    query_id: Some(query_id),
                    reason,
                    cancellation,
                })
            }
        }
    }

    async fn poll_query(
        &self,
        client: &Client,
        query_id: &str,
        destination: &QueryDestination,
        guard: &RequestGuard<'_>,
        options: &QueryOptions,
    ) -> Result<QueryExecution, PollError> {
        loop {
            let response = guard
                .run(async {
                    client
                        .get_query_execution()
                        .query_execution_id(query_id)
                        .send()
                        .await
                        .map_err(|error| QueryInterruption::RequestFailed {
                            operation: "GetQueryExecution",
                            code: error
                                .as_service_error()
                                .and_then(|error| error.code())
                                .unwrap_or("request failed")
                                .to_owned(),
                            message: crate::error::service_message(error.as_service_error()),
                        })
                })
                .await?;
            let execution = response
                .query_execution()
                .ok_or(QueryInterruption::InvalidResponse("query execution"))?;
            if execution.query_execution_id() != Some(query_id)
                || execution.work_group() != Some(self.options().workgroup.as_str())
            {
                return Err(QueryInterruption::InvalidResponse("query identity").into());
            }
            let status = execution
                .status()
                .ok_or(QueryInterruption::InvalidResponse("query status"))?;
            match status.state() {
                Some(QueryExecutionState::Succeeded) => {
                    let manifest = execution
                        .statistics()
                        .and_then(|stats| stats.data_manifest_location())
                        .filter(|location| !location.is_empty())
                        .map(|location| {
                            if location.starts_with(&destination.output_prefix)
                                && s3_location(location)
                            {
                                Ok(location.to_owned())
                            } else {
                                Err(QueryInterruption::InvalidResponse("manifest location"))
                            }
                        })
                        .transpose()?;
                    let actual = execution
                        .result_configuration()
                        .filter(|actual| {
                            actual.encryption_configuration()
                                == destination.configuration.encryption_configuration()
                                && actual.expected_bucket_owner()
                                    == destination.configuration.expected_bucket_owner()
                                && actual.output_location().is_some_and(|location| {
                                    location.starts_with(&destination.output_prefix)
                                        && s3_location(location)
                                })
                        })
                        .ok_or(QueryInterruption::InvalidResponse(
                            "query result configuration",
                        ))?;
                    return Ok(QueryExecution {
                        query_id: query_id.to_owned(),
                        manifest_location: manifest,
                        data_prefix: destination.data_prefix.clone(),
                        result_configuration: actual.clone(),
                    });
                }
                Some(QueryExecutionState::Failed) => {
                    return Err(PollError::Failed(
                        status
                            .state_change_reason()
                            .unwrap_or("Athena reported failure without a reason")
                            .to_owned(),
                    ));
                }
                Some(QueryExecutionState::Cancelled) => return Err(PollError::Cancelled),
                Some(QueryExecutionState::Queued | QueryExecutionState::Running) => {}
                _ => return Err(QueryInterruption::InvalidResponse("query state").into()),
            }
            guard
                .run(async {
                    sleep(options.poll_interval).await;
                    Ok(())
                })
                .await?;
        }
    }

    async fn stop_query(&self, client: &Client, query_id: &str) -> CancellationStatus {
        if self.session_status().is_err() {
            return CancellationStatus::Unconfirmed;
        }
        match timeout(
            STOP_TIMEOUT,
            client
                .stop_query_execution()
                .query_execution_id(query_id)
                .send(),
        )
        .await
        {
            Ok(Ok(_)) => CancellationStatus::Requested,
            _ => CancellationStatus::Unconfirmed,
        }
    }
}

struct QueryDestination {
    data_prefix: String,
    output_prefix: String,
    configuration: ResultConfiguration,
}

pub(crate) struct RequestGuard<'a> {
    pub(crate) connection: &'a Connection,
    pub(crate) cancellation: &'a QueryCancellation,
    pub(crate) deadline: Instant,
}

impl RequestGuard<'_> {
    pub(crate) fn check(&self) -> Result<(), QueryInterruption> {
        if self.cancellation.is_cancelled() {
            return Err(QueryInterruption::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(QueryInterruption::TimedOut);
        }
        self.connection
            .session_status()
            .map(|_| ())
            .map_err(|_| QueryInterruption::SessionExpired)
    }

    pub(crate) async fn run<T>(
        &self,
        request: impl Future<Output = Result<T, QueryInterruption>>,
    ) -> Result<T, QueryInterruption> {
        self.check()?;
        let role_deadline = match self
            .connection
            .session_status()
            .map_err(|_| QueryInterruption::SessionExpired)?
        {
            SessionStatus::Standard => None,
            SessionStatus::Assumed { remaining, .. }
            | SessionStatus::Expiring { remaining, .. } => Some(Instant::now() + remaining),
        };
        tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => Err(QueryInterruption::Cancelled),
            _ = sleep_until(self.deadline) => Err(QueryInterruption::TimedOut),
            _ = async { match role_deadline { Some(deadline) => sleep_until(deadline).await, None => std::future::pending().await } } => Err(QueryInterruption::SessionExpired),
            result = request => result,
        }
    }
}

fn before_submission(reason: QueryInterruption) -> Error {
    Error::QueryInterrupted {
        query_id: None,
        reason,
        cancellation: CancellationStatus::NotSubmitted,
    }
}

enum PollError {
    Interrupted(QueryInterruption),
    Failed(String),
    Cancelled,
}
impl From<QueryInterruption> for PollError {
    fn from(reason: QueryInterruption) -> Self {
        Self::Interrupted(reason)
    }
}

pub(crate) fn query_text(sql: &str) -> Result<&str, Error> {
    // Remove only surrounding whitespace and one terminal delimiter. The SQL
    // body, including comments and dialect-specific syntax, belongs to Athena.
    let query = sql
        .trim()
        .strip_suffix(';')
        .unwrap_or(sql.trim())
        .trim_end();
    if query.is_empty() {
        Err(Error::InvalidQuery("provide a query"))
    } else {
        Ok(query)
    }
}

fn s3_location(location: &str) -> bool {
    crate::location::S3Location::parse(location).is_some()
}

fn s3_prefix(location: &str) -> Option<String> {
    s3_location(location).then(|| {
        if location.ends_with('/') {
            location.to_owned()
        } else {
            format!("{location}/")
        }
    })
}

#[cfg(test)]
mod tests;
