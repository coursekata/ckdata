use std::collections::HashSet;
use std::sync::atomic::AtomicUsize;

use aws_sdk_athena::types::{EncryptionOption, ResultConfiguration};
use aws_sdk_s3::error::ProvideErrorMetadata;
use aws_sdk_s3::types::ServerSideEncryption;
use bytes::{Bytes, BytesMut};

use super::{Budget, ResultError, ResultLimits, reserve};
use crate::location::S3Location;
use crate::query::RequestGuard;
use crate::{Connection, QueryExecution, QueryInterruption};

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
    let client = aws_sdk_s3::Client::new(connection.aws_config());
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
                ) && expected.kms_key().is_some_and(|key| {
                    key.starts_with("arn:")
                        && key.contains(":key/")
                        && response.ssekms_key_id() == Some(key)
                })
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
                body.next()
                    .await
                    .transpose()
                    .map_err(|_| QueryInterruption::RequestFailed {
                        operation: "GetObject body",
                        code: "response stream failed".into(),
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
