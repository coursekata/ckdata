/// Errors contain no credential values or raw AWS authentication responses.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("invalid connection setting: {0}")]
    InvalidSetting(&'static str),
    #[error("no AWS region was resolved; configure a region or supply one explicitly")]
    MissingRegion,
    #[error("AWS credentials could not be loaded; check the selected profile and AWS sign-in")]
    CredentialsUnavailable,
    #[error("AWS SSO sign-in is required for profile {profile}", profile = .target.profile)]
    SsoLoginRequired { target: crate::SsoLoginTarget },
    #[error("assuming the requested role failed ({code}){detail}", detail = message_suffix(.message))]
    AssumeRole {
        code: String,
        message: Option<String>,
    },
    #[error("STS returned incomplete or expired role credentials")]
    InvalidRoleCredentials,
    #[error("the assumed-role session expired; create a new connection")]
    ExpiredRole,
    #[error("AWS {operation} failed ({code}){detail}", detail = message_suffix(.message))]
    Service {
        operation: &'static str,
        code: String,
        message: Option<String>,
    },
    #[error("AWS {0} returned an incomplete response")]
    InvalidServiceResponse(&'static str),
    #[error("unsupported SQL: {0}")]
    InvalidQuery(&'static str),
    #[error("Athena query submission failed ({code}){detail}; request token {request_token}", detail = message_suffix(.message))]
    QuerySubmission {
        code: String,
        message: Option<String>,
        request_token: String,
        outcome_unknown: bool,
    },
    #[error("Athena query {query_id} failed: {reason}")]
    QueryFailed { query_id: String, reason: String },
    #[error("Athena query {query_id:?} interrupted: {reason}; cancellation {cancellation}")]
    QueryInterrupted {
        query_id: Option<String>,
        reason: crate::QueryInterruption,
        cancellation: crate::CancellationStatus,
    },
    #[error("Athena query {query_id} result: {reason}")]
    Result {
        query_id: String,
        reason: crate::ResultError,
    },
    #[error("Athena query {query_id} empty-result schema recovery: {source}")]
    SchemaRecovery {
        query_id: String,
        #[source]
        source: Box<Error>,
    },
}

impl Error {
    /// Whether a query may still be running after this failure. Hosts should
    /// preserve this warning even when another condition caused their cleanup.
    pub fn remote_query_may_be_running(&self) -> bool {
        match self {
            Self::QuerySubmission {
                outcome_unknown, ..
            } => *outcome_unknown,
            Self::QueryInterrupted { cancellation, .. } => {
                *cancellation == crate::CancellationStatus::Unconfirmed
            }
            Self::SchemaRecovery { source, .. } => source.remote_query_may_be_running(),
            _ => false,
        }
    }

    /// Retain cancellation status through nested empty-schema recovery errors.
    pub fn cancellation_status(&self) -> Option<crate::CancellationStatus> {
        match self {
            Self::QueryInterrupted { cancellation, .. } => Some(*cancellation),
            Self::SchemaRecovery { source, .. } => source.cancellation_status(),
            _ => None,
        }
    }
}

/// Copy modeled service guidance, excluding authentication diagnostics that can
/// contain signed request details. Credential-provider failures stay generic.
pub(crate) fn service_message(
    service: Option<&impl aws_sdk_athena::error::ProvideErrorMetadata>,
) -> Option<String> {
    let service = service?;
    let code = service.code()?.to_ascii_lowercase();
    if ["signature", "token", "credential", "auth", "accesskey"]
        .iter()
        .any(|part| code.contains(part))
        || matches!(
            code.as_str(),
            "unrecognizedclientexception" | "requestexpired" | "invalidsecurity"
        )
    {
        return None;
    }
    service
        .message()
        .filter(|message| !message.is_empty())
        .map(str::to_owned)
}

pub(crate) fn message_suffix(message: &Option<String>) -> String {
    message
        .as_deref()
        .map(|message| format!(": {message}"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_smithy_types::error::ErrorMetadata;

    #[test]
    fn cancellation_metadata_survives_schema_recovery() {
        for cancellation in [
            crate::CancellationStatus::NotSubmitted,
            crate::CancellationStatus::Requested,
            crate::CancellationStatus::Unconfirmed,
        ] {
            let error = Error::SchemaRecovery {
                query_id: "original".into(),
                source: Box::new(Error::QueryInterrupted {
                    query_id: Some("recovery".into()),
                    reason: crate::QueryInterruption::Cancelled,
                    cancellation,
                }),
            };
            assert_eq!(error.cancellation_status(), Some(cancellation));
            assert_eq!(
                error.remote_query_may_be_running(),
                cancellation == crate::CancellationStatus::Unconfirmed
            );
        }
    }

    #[test]
    fn submission_uncertainty_is_independent_of_service_message() {
        for outcome_unknown in [false, true] {
            let error = Error::QuerySubmission {
                code: "synthetic".into(),
                message: Some("outcome unknown in user-facing guidance".into()),
                request_token: "synthetic".into(),
                outcome_unknown,
            };
            assert_eq!(error.remote_query_may_be_running(), outcome_unknown);
            assert_eq!(error.cancellation_status(), None);
        }
    }

    #[test]
    fn modeled_guidance_is_displayed_without_exposing_authentication_details() {
        for code in ["AccessDenied", "InvalidRequestException", "NoSuchKey"] {
            let metadata = ErrorMetadata::builder()
                .code(code)
                .message("check the configured output location")
                .build();
            let error = Error::Service {
                operation: "test",
                code: code.into(),
                message: service_message(Some(&metadata)),
            };
            assert!(
                error
                    .to_string()
                    .contains("check the configured output location")
            );
        }
        for code in [
            "SignatureDoesNotMatch",
            "InvalidSignatureException",
            "IncompleteSignature",
            "InvalidClientTokenId",
            "ExpiredTokenException",
            "UnrecognizedClientException",
            "RequestExpired",
            "InvalidSecurity",
        ] {
            let metadata = ErrorMetadata::builder()
                .code(code)
                .message("synthetic signed authentication detail")
                .build();
            let error = Error::Service {
                operation: "test",
                code: code.into(),
                message: service_message(Some(&metadata)),
            };
            assert!(matches!(&error, Error::Service { message: None, .. }));
            assert!(
                !format!("{error:?} {error}").contains("synthetic signed authentication detail")
            );
        }
    }
}
