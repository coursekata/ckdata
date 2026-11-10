use std::future::Future;
use std::sync::{Arc, OnceLock};

use aws_sdk_athena::config::interceptors::BeforeTransmitInterceptorContextRef;
use aws_sdk_athena::config::{ConfigBag, Intercept, RuntimeComponents};
use tokio::sync::Notify;
use tokio::time::{Instant, sleep_until, timeout_at};

use super::{REQUEST_TIMEOUT, RequestGuard, before_submission};
use crate::Error;

/// The browser may need longer than a network request. Observe the SDK's first
/// transmission so its retries and response wait share one short network bound.
#[derive(Clone, Debug, Default)]
pub(super) struct Submission {
    sent: Arc<OnceLock<Instant>>,
    changed: Arc<Notify>,
}

impl Intercept for Submission {
    fn name(&self) -> &'static str {
        "ckdata-submission"
    }

    fn read_before_transmit(
        &self,
        _: &BeforeTransmitInterceptorContextRef<'_>,
        _: &RuntimeComponents,
        _: &mut ConfigBag,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.sent.get_or_init(Instant::now);
        self.changed.notify_one();
        Ok(())
    }
}

impl Submission {
    pub(super) fn sent(&self) -> bool {
        self.sent.get().is_some()
    }

    pub(super) async fn wait<T>(
        &self,
        request: impl Future<Output = Result<T, Error>>,
        guard: &RequestGuard<'_>,
        request_token: &str,
    ) -> Result<T, Error> {
        tokio::pin!(request);
        loop {
            let changed = self.changed.notified();
            if let Some(sent) = self.sent.get() {
                // Retain a possibly accepted query's ID even after cancellation,
                // without extending either the query deadline or network bound.
                return timeout_at((*sent + REQUEST_TIMEOUT).min(guard.deadline), request)
                    .await
                    .unwrap_or_else(|_| {
                        Err(Error::QuerySubmission {
                            outcome_unknown: true,
                            code: "submission timed out; submission outcome unknown".into(),
                            message: None,
                            request_token: request_token.into(),
                        })
                    });
            }
            tokio::select! {
                biased;
                _ = changed => {},
                _ = guard.cancellation.cancelled() => {
                    if !self.sent() {
                        return Err(before_submission(super::QueryInterruption::Cancelled));
                    }
                },
                _ = sleep_until(guard.deadline) => {
                    if !self.sent() {
                        return Err(before_submission(super::QueryInterruption::TimedOut));
                    }
                },
                result = &mut request => return result,
            }
        }
    }
}
