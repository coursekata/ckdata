use std::collections::HashSet;
use std::future::Future;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

use aws_sdk_athena::types::{EncryptionOption, ResultConfiguration};
use aws_sdk_s3::config::timeout::TimeoutConfig;
use aws_sdk_s3::error::ProvideErrorMetadata;
use aws_sdk_s3::types::ServerSideEncryption;
use bytes::{Bytes, BytesMut};
use tokio::time::timeout;
use uuid::Uuid;

use super::{Budget, ResultError, ResultLimits, reserve};
use crate::location::S3Location;
use crate::query::RequestGuard;
use crate::{Connection, QueryExecution, QueryInterruption};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Aliases cannot be checked against S3's resolved key ID without KMS access.
/// Require a key ARN or canonical UUID so encryption can be verified locally.
pub(crate) fn valid_kms_key(key: &str) -> bool {
    key_uuid(key).is_some() || key_id_from_arn(key).is_some()
}

fn key_uuid(key: &str) -> Option<Uuid> {
    let uuid = Uuid::parse_str(key).ok()?;
    (uuid.hyphenated().to_string().eq_ignore_ascii_case(key)).then_some(uuid)
}

fn key_id_from_arn(arn: &str) -> Option<&str> {
    let mut parts = arn.splitn(6, ':');
    if parts.next()? != "arn"
        || parts.next()?.is_empty()
        || parts.next()? != "kms"
        || parts.next()?.is_empty()
    {
        return None;
    }
    let account = parts.next()?;
    if account.len() != 12 || !account.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let key = parts.next()?.strip_prefix("key/")?;
    let multi_region = key
        .strip_prefix("mrk-")
        .is_some_and(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()));
    (key_uuid(key).is_some() || multi_region).then_some(key)
}

fn kms_key_matches(expected: &str, actual: Option<&str>) -> bool {
    let Some(actual) = actual else {
        return false;
    };
    if key_id_from_arn(expected).is_some() {
        // Keep the account, region and partition constraint of a configured ARN.
        return expected == actual;
    }
    let Some(expected) = key_uuid(expected) else {
        return false;
    };
    key_uuid(actual).or_else(|| key_id_from_arn(actual).and_then(key_uuid)) == Some(expected)
}

async fn read_body_chunk<T>(read: impl Future<Output = T>) -> Result<T, QueryInterruption> {
    timeout(REQUEST_TIMEOUT, read)
        .await
        .map_err(|_| QueryInterruption::RequestFailed {
            operation: "GetObject body",
            code: "request timed out".into(),
            message: None,
        })
}

pub(super) async fn manifest(
    connection: &Connection,
    execution: &QueryExecution,
    limits: &ResultLimits,
    guard: &RequestGuard<'_>,
) -> Result<Vec<S3Location>, ResultError> {
    let location = S3Location::parse(&execution.manifest_location)
        .ok_or(ResultError::InvalidManifest("invalid manifest location"))?;
    let bytes = object(
        connection,
        &location,
        &execution.result_configuration,
        limits.max_manifest_bytes,
        &AtomicUsize::new(0),
        limits.max_manifest_bytes,
        guard,
    )
    .await?;
    parse_manifest(&bytes, &execution.data_prefix, limits.max_files)
}

fn parse_manifest(
    bytes: &[u8],
    prefix: &str,
    max_files: usize,
) -> Result<Vec<S3Location>, ResultError> {
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(false)
        .from_reader(bytes);
    for record in reader.records() {
        let record = record.map_err(|_| ResultError::InvalidManifest("invalid CSV"))?;
        if record.len() != 1 {
            return Err(ResultError::InvalidManifest("expected one URI per row"));
        }
        let uri = &record[0];
        if files.len() >= max_files {
            return Err(ResultError::LimitExceeded("manifest file count"));
        }
        if !uri.starts_with(prefix) {
            return Err(ResultError::InvalidManifest(
                "file is outside this query's output prefix",
            ));
        }
        let file = S3Location::parse(uri)
            .filter(|file| !file.key.ends_with('/'))
            .ok_or(ResultError::InvalidManifest("invalid data file URI"))?;
        if !seen.insert(uri.to_owned()) {
            return Err(ResultError::InvalidManifest("duplicate data file"));
        }
        files.push(file);
    }
    if files.is_empty() {
        return Err(ResultError::InvalidManifest("no schema-bearing files"));
    }
    Ok(files)
}

pub(super) async fn download(
    connection: &Connection,
    location: &S3Location,
    execution: &QueryExecution,
    limits: &ResultLimits,
    guard: &RequestGuard<'_>,
    budget: &Budget,
) -> Result<Bytes, ResultError> {
    object(
        connection,
        location,
        &execution.result_configuration,
        limits.max_file_bytes,
        &budget.downloaded,
        limits.max_download_bytes,
        guard,
    )
    .await
}

async fn object(
    connection: &Connection,
    location: &S3Location,
    configuration: &ResultConfiguration,
    file_limit: usize,
    total: &AtomicUsize,
    total_limit: usize,
    guard: &RequestGuard<'_>,
) -> Result<Bytes, ResultError> {
    let client = aws_sdk_s3::Client::from_conf(
        aws_sdk_s3::config::Builder::from(connection.aws_config())
            .timeout_config(
                TimeoutConfig::builder()
                    .operation_timeout(REQUEST_TIMEOUT)
                    .build(),
            )
            .build(),
    );
    let response = guard
        .run(async {
            client
                .get_object()
                .bucket(&location.bucket)
                .key(&location.key)
                .set_expected_bucket_owner(configuration.expected_bucket_owner().map(str::to_owned))
                .send()
                .await
                .map_err(|error| QueryInterruption::RequestFailed {
                    operation: "GetObject",
                    code: error
                        .as_service_error()
                        .and_then(|error| error.code())
                        .unwrap_or("request failed")
                        .to_owned(),
                    message: crate::error::service_message(error.as_service_error()),
                })
        })
        .await
        .map_err(ResultError::Interrupted)?;
    let encryption = response.server_side_encryption();
    let encrypted = match configuration.encryption_configuration() {
        Some(expected) => match expected.encryption_option() {
            EncryptionOption::SseS3 => encryption == Some(&ServerSideEncryption::Aes256),
            EncryptionOption::SseKms => {
                matches!(
                    encryption,
                    Some(ServerSideEncryption::AwsKms | ServerSideEncryption::AwsKmsDsse)
                ) && expected
                    .kms_key()
                    .is_some_and(|key| kms_key_matches(key, response.ssekms_key_id()))
            }
            _ => false,
        },
        None => matches!(
            encryption,
            Some(
                ServerSideEncryption::Aes256
                    | ServerSideEncryption::AwsKms
                    | ServerSideEncryption::AwsKmsDsse
            )
        ),
    };
    if !encrypted {
        return Err(ResultError::EncryptionMismatch);
    }
    let expected_len = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .ok_or(ResultError::InvalidParquet(
            "object has no valid content length",
        ))?;
    if expected_len > file_limit {
        return Err(ResultError::LimitExceeded("object bytes"));
    }
    let mut body = response.body;
    let mut bytes = BytesMut::new();
    loop {
        let chunk = guard
            .run(async {
                read_body_chunk(body.next())
                    .await?
                    .transpose()
                    .map_err(|_| QueryInterruption::RequestFailed {
                        operation: "GetObject body",
                        code: "response stream failed".into(),
                        message: None,
                    })
            })
            .await
            .map_err(ResultError::Interrupted)?;
        let Some(chunk) = chunk else {
            break;
        };
        if bytes
            .len()
            .checked_add(chunk.len())
            .is_none_or(|length| length > file_limit)
        {
            return Err(ResultError::LimitExceeded("object bytes"));
        }
        reserve(total, chunk.len(), total_limit, "download bytes")?;
        bytes.extend_from_slice(&chunk);
    }
    if bytes.len() != expected_len {
        return Err(ResultError::InvalidParquet("incomplete object body"));
    }
    Ok(bytes.freeze())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kms_settings_require_a_key_arn_or_standard_uuid() {
        for key in [
            "12345678-1234-1234-1234-123456789abc",
            "arn:aws:kms:us-west-2:000000000000:key/12345678-1234-1234-1234-123456789abc",
            "arn:aws-us-gov:kms:us-gov-west-1:000000000000:key/mrk-0123456789abcdef0123456789abcdef",
        ] {
            assert!(valid_kms_key(key), "{key}");
        }
        for key in [
            "",
            "alias/synthetic",
            "12345678123412341234123456789abc",
            "{12345678-1234-1234-1234-123456789abc}",
            "arn:aws:kms:us-west-2:000000000000:alias/synthetic",
            "arn:aws:s3:us-west-2:000000000000:key/12345678-1234-1234-1234-123456789abc",
            "arn:aws:kms::000000000000:key/12345678-1234-1234-1234-123456789abc",
            "arn:aws:kms:us-west-2:invalid:key/12345678-1234-1234-1234-123456789abc",
            "arn:aws:kms:us-west-2:000000000000:key/not-a-uuid",
        ] {
            assert!(!valid_kms_key(key), "{key}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_body_read_stops_at_ten_seconds() {
        let start = tokio::time::Instant::now();
        assert!(matches!(
            read_body_chunk(std::future::pending::<()>()).await,
            Err(QueryInterruption::RequestFailed {
                operation: "GetObject body",
                ..
            })
        ));
        assert_eq!(start.elapsed(), REQUEST_TIMEOUT);
    }

    #[tokio::test(start_paused = true)]
    async fn get_object_is_capped_even_when_the_sdk_config_allows_longer_requests() {
        use crate::{ConnectionOptions, QueryCancellation};
        use aws_config::{BehaviorVersion, retry::RetryConfig};
        use aws_credential_types::{Credentials, provider::SharedCredentialsProvider};
        use aws_smithy_async::rt::sleep::TokioSleep;
        use aws_smithy_http_client::test_util::NeverClient;
        use aws_types::{SdkConfig, region::Region};
        let never = NeverClient::new();
        let config = SdkConfig::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new("us-west-2"))
            .credentials_provider(SharedCredentialsProvider::new(Credentials::new(
                "SYNTHETIC",
                "synthetic-secret",
                None,
                None,
                "test",
            )))
            .retry_config(RetryConfig::standard().with_max_attempts(1))
            .sleep_impl(TokioSleep::new())
            .timeout_config(
                TimeoutConfig::builder()
                    .operation_timeout(Duration::from_secs(120))
                    .build(),
            )
            .http_client(never.clone())
            .build();
        let connection =
            Connection::from_config(ConnectionOptions::new("synthetic", "schema"), config)
                .await
                .unwrap();
        let cancellation = QueryCancellation::new();
        let start = tokio::time::Instant::now();
        let guard = RequestGuard {
            connection: &connection,
            cancellation: &cancellation,
            deadline: start + Duration::from_secs(60),
        };
        let result = object(
            &connection,
            &S3Location::parse("s3://synthetic/manifest.csv").unwrap(),
            &ResultConfiguration::builder().build(),
            1024,
            &AtomicUsize::new(0),
            1024,
            &guard,
        )
        .await;
        assert!(matches!(
            result,
            Err(ResultError::Interrupted(QueryInterruption::RequestFailed {
                operation: "GetObject",
                ..
            }))
        ));
        assert_eq!(never.num_calls(), 1);
        assert_eq!(start.elapsed(), REQUEST_TIMEOUT);

        let start = tokio::time::Instant::now();
        let guard = RequestGuard {
            deadline: start + Duration::from_secs(2),
            ..guard
        };
        let result = guard
            .run(async { read_body_chunk(std::future::pending::<()>()).await })
            .await;
        assert_eq!(result, Err(QueryInterruption::TimedOut));
        assert_eq!(start.elapsed(), Duration::from_secs(2));
    }

    #[test]
    fn rejects_malformed_duplicate_and_escaping_manifest_entries() {
        let prefix = "s3://synthetic/query/";
        let files = parse_manifest(
            b"\"s3://synthetic/query/part-1\"\r\ns3://synthetic/query/part-2\n",
            prefix,
            2,
        )
        .unwrap();
        assert_eq!(files.len(), 2);
        for bytes in [
            b"".as_slice(),
            b"s3://other/query/part\n",
            b"s3://synthetic/query/../part\n",
            b"s3://synthetic/query/%2e%2e/part\n",
            b"s3://synthetic/query/a\ns3://synthetic/query/a\n",
            b"s3://synthetic/query/a,extra\n",
        ] {
            assert!(parse_manifest(bytes, prefix, 2).is_err());
        }
        assert!(matches!(
            parse_manifest(
                b"s3://synthetic/query/a\ns3://synthetic/query/b\n",
                prefix,
                1
            ),
            Err(ResultError::LimitExceeded(_))
        ));
    }
}
