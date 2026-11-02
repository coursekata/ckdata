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
    #[error("assuming the requested role failed ({code})")]
    AssumeRole { code: String },
    #[error("STS returned incomplete or expired role credentials")]
    InvalidRoleCredentials,
    #[error("the assumed-role session expired; create a new connection")]
    ExpiredRole,
    #[error("AWS {operation} failed ({code})")]
    Service {
        operation: &'static str,
        code: String,
    },
    #[error("AWS {0} returned an incomplete response")]
    InvalidServiceResponse(&'static str),
}
