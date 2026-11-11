//! Shared connection and Arrow infrastructure for ckdata's R and Python bindings.
//! `Connection::connect` explicitly resolves AWS credentials and may assume a role.
mod auth;
pub mod column_policy;
mod connection;
mod error;
mod location;
mod metadata;
mod query;
mod result;
mod settings;

pub use auth::{SsoLogin, SsoLoginTarget, SsoOptions};
pub use connection::{CallerIdentity, Connection};
pub use error::Error;
pub use metadata::{ColumnMetadata, TableMetadata, TablePage};
pub use query::{CancellationStatus, QueryExecution, QueryInterruption, QueryOptions};
pub use result::{QueryResult, ResultError};
pub use settings::ConnectionOptions;
pub use tokio_util::sync::CancellationToken as QueryCancellation;

/// The core version, then the Athena and S3 SDK versions.
pub fn versions() -> Vec<String> {
    vec![
        format!("ckdata {}", env!("CARGO_PKG_VERSION")),
        format!("aws-sdk-athena {}", aws_sdk_athena::meta::PKG_VERSION),
        format!("aws-sdk-s3 {}", aws_sdk_s3::meta::PKG_VERSION),
    ]
}
