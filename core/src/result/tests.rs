use super::*;
use std::sync::Mutex;

use arrow_array::types::{Int64Type, TimestampMillisecondType};
use arrow_array::{
    Array, ArrayRef, BooleanArray, Decimal128Array, Int64Array, ListArray, PrimitiveArray,
    StringArray, new_null_array,
};
use arrow_schema::{DataType, Field, Schema};
use aws_config::{BehaviorVersion, retry::RetryConfig};
use aws_credential_types::{Credentials, provider::SharedCredentialsProvider};
use aws_smithy_async::time::StaticTimeSource;
use aws_smithy_http_client::test_util::infallible_client_fn;
use aws_types::{SdkConfig, region::Region};
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use serde_json::{Value, json};

use crate::ConnectionOptions;

const OUTPUT: &str = "s3://synthetic-results/results/";
const SQL: &str = "SELECT label, n, s_null, s_empty, ts, dec, arr FROM synthetic ORDER BY label ASC NULLS LAST, n DESC";

#[derive(Clone, Copy)]
enum Mode {
    Rows,
    Empty,
    AllNull,
    BadManifest,
    BadEncryption,
    MissingFile,
    MismatchedSchema,
    Timezone,
    BadPresence,
    NoFiles,
    CompactRows,
    Expiring,
}

#[derive(Clone, Copy)]
enum Encryption {
    SseS3,
    Kms {
        configured: Option<&'static str>,
        actual: Option<&'static str>,
    },
}

const KEY_UUID: &str = "12345678-1234-1234-1234-123456789abc";
const KEY_ARN: &str = "arn:aws:kms:us-west-2:000000000000:key/12345678-1234-1234-1234-123456789abc";
const OTHER_KEY: &str =
    "arn:aws:kms:us-west-2:000000000000:key/87654321-4321-4321-4321-cba987654321";
const OTHER_ACCOUNT_KEY: &str =
    "arn:aws:kms:us-west-2:111111111111:key/12345678-1234-1234-1234-123456789abc";

#[derive(Default)]
struct State {
    requests: Vec<String>,
    prefix: String,
    files: Vec<Vec<u8>>,
}

fn batch(marker: &str, part: usize, mode: Mode) -> RecordBatch {
    if matches!(mode, Mode::CompactRows) {
        return RecordBatch::try_from_iter(vec![
            (
                "label",
                Arc::new(StringArray::from(vec!["repeated"; 128])) as ArrayRef,
            ),
            (
                marker,
                Arc::new(BooleanArray::from(vec![true; 128])) as ArrayRef,
            ),
        ])
        .unwrap();
    }
    let timestamp = if matches!(mode, Mode::Timezone) {
        DataType::Timestamp(arrow_schema::TimeUnit::Millisecond, Some("UTC".into()))
    } else {
        DataType::Timestamp(arrow_schema::TimeUnit::Millisecond, None)
    };
    let nested = DataType::List(Arc::new(Field::new("item", DataType::Int64, true)));
    let n_type = if matches!(mode, Mode::MismatchedSchema) && part == 1 {
        DataType::Utf8
    } else {
        DataType::Int64
    };
    let schema = Arc::new(Schema::new(vec![
        Field::new("label", DataType::Utf8, true),
        Field::new("n", n_type, true),
        Field::new("s_null", DataType::Utf8, true),
        Field::new("s_empty", DataType::Utf8, true),
        Field::new("ts", timestamp.clone(), true),
        Field::new("dec", DataType::Decimal128(10, 2), true),
        Field::new("arr", nested, true),
        Field::new(marker, DataType::Boolean, true),
    ]));
    if matches!(mode, Mode::Empty | Mode::AllNull) {
        let rows = if matches!(mode, Mode::Empty) { 1 } else { 2 };
        let mut columns: Vec<_> = schema.fields()[..7]
            .iter()
            .map(|field| new_null_array(field.data_type(), rows))
            .collect();
        columns.push(Arc::new(BooleanArray::from(vec![
            if matches!(mode, Mode::Empty) {
                None
            } else {
                Some(true)
            };
            rows
        ])));
        return RecordBatch::try_new(schema, columns).unwrap();
    }
    let labels = if part == 0 {
        vec![Some("b"), None]
    } else {
        vec![Some("a"), Some("a")]
    };
    let n: ArrayRef = if matches!(mode, Mode::MismatchedSchema) && part == 1 {
        Arc::new(StringArray::from(vec!["3", "1"]))
    } else {
        Arc::new(Int64Array::from(if part == 0 {
            vec![9_007_199_254_740_993, 9]
        } else {
            vec![3, 1]
        }))
    };
    let ts = PrimitiveArray::<TimestampMillisecondType>::from(vec![Some(1_792_154_096_789), None]);
    let ts = if matches!(mode, Mode::Timezone) {
        ts.with_timezone("UTC")
    } else {
        ts
    };
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(labels)),
            n,
            Arc::new(StringArray::from(vec![None, Some("a")])),
            Arc::new(StringArray::from(vec![Some(""), None])),
            Arc::new(ts),
            Arc::new(
                Decimal128Array::from(vec![Some(110), None])
                    .with_precision_and_scale(10, 2)
                    .unwrap(),
            ),
            Arc::new(ListArray::from_iter_primitive::<Int64Type, _, _>(vec![
                Some(vec![Some(1), None]),
                None,
            ])),
            Arc::new(BooleanArray::from(vec![
                Some(!matches!(mode, Mode::BadPresence)),
                Some(true),
            ])),
        ],
    )
    .unwrap()
}

fn parquet(batch: &RecordBatch) -> Vec<u8> {
    let mut bytes = Vec::new();
    let properties = WriterProperties::builder()
        .set_compression(Compression::SNAPPY)
        .build();
    let mut writer = ArrowWriter::try_new(&mut bytes, batch.schema(), Some(properties)).unwrap();
    writer.write(batch).unwrap();
    writer.close().unwrap();
    bytes
}

async fn service(mode: Mode, token: Option<QueryCancellation>) -> (Connection, Arc<Mutex<State>>) {
    service_with_encryption(mode, token, Encryption::SseS3).await
}

async fn service_with_encryption(
    mode: Mode,
    token: Option<QueryCancellation>,
    encryption: Encryption,
) -> (Connection, Arc<Mutex<State>>) {
    let state = Arc::new(Mutex::new(State::default()));
    let observed = state.clone();
    let http = infallible_client_fn(move |request| {
        let mut state = observed.lock().unwrap();
        if request.method() == http::Method::POST && request.headers().get("x-amz-target").is_none()
        {
            assert!(matches!(mode, Mode::Expiring));
            state.requests.push("AssumeRole".into());
            let response = r#"<AssumeRoleResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/"><AssumeRoleResult><Credentials><AccessKeyId>ASSUMED_TEST</AccessKeyId><SecretAccessKey>synthetic</SecretAccessKey><SessionToken>synthetic</SessionToken><Expiration>2024-01-01T00:10:00Z</Expiration></Credentials></AssumeRoleResult></AssumeRoleResponse>"#;
            return http::Response::builder()
                .status(200)
                .header("content-type", "text/xml")
                .header("content-length", response.len())
                .body(response.as_bytes().to_vec())
                .unwrap();
        }
        let target = request.headers().get("x-amz-target").map(|value| {
            value
                .to_str()
                .unwrap()
                .rsplit('.')
                .next()
                .unwrap()
                .to_owned()
        });
        let mut configuration = json!({"OutputLocation": OUTPUT, "EncryptionConfiguration": {"EncryptionOption": "SSE_S3"}, "ExpectedBucketOwner": "000000000000"});
        if let Encryption::Kms { configured, .. } = encryption {
            configuration["EncryptionConfiguration"] = json!({"EncryptionOption": "SSE_KMS"});
            if let Some(key) = configured {
                configuration["EncryptionConfiguration"]["KmsKey"] = json!(key);
            }
        }
        let (status, body, encrypted) = if let Some(target) = target {
            state.requests.push(target.clone());
            let body = match target.as_str() {
                "GetWorkGroup" => {
                    json!({"WorkGroup": {"Name": "synthetic", "State": "ENABLED", "Configuration": {"ResultConfiguration": configuration}}})
                }
                "StartQueryExecution" => {
                    let body: Value =
                        serde_json::from_slice(request.body().bytes().unwrap()).unwrap();
                    let sql = body["QueryString"].as_str().unwrap();
                    assert!(sql.contains("LEFT JOIN") && sql.contains("ckdata_rows.*"));
                    let marker = format!(
                        "__ckdata_presence_{}",
                        sql.split("__ckdata_presence_")
                            .nth(1)
                            .unwrap()
                            .chars()
                            .take(32)
                            .collect::<String>()
                    );
                    state.prefix = sql
                        .split(" TO '")
                        .nth(1)
                        .unwrap()
                        .split('\'')
                        .next()
                        .unwrap()
                        .to_owned();
                    state.files = vec![parquet(&batch(&marker, 0, mode))];
                    if !matches!(mode, Mode::Empty | Mode::AllNull | Mode::CompactRows) {
                        state.files.push(parquet(&batch(&marker, 1, mode)));
                    }
                    json!({"QueryExecutionId": "synthetic-query"})
                }
                "GetQueryExecution" => {
                    let mut actual = configuration;
                    actual["OutputLocation"] = json!(format!("{OUTPUT}synthetic-query.txt"));
                    json!({"QueryExecution": {"QueryExecutionId": "synthetic-query", "WorkGroup": "synthetic", "Status": {"State": "SUCCEEDED"}, "Statistics": {"DataManifestLocation": format!("{OUTPUT}synthetic-query-manifest.csv")}, "ResultConfiguration": actual}})
                }
                _ => panic!("unexpected operation {target}"),
            };
            (200, body.to_string().into_bytes(), true)
        } else {
            assert_eq!(request.method(), http::Method::GET);
            assert_eq!(
                request.headers()["x-amz-expected-bucket-owner"],
                "000000000000"
            );
            let path = request.uri().path();
            state.requests.push(format!("GET {path}"));
            if let Some(token) = &token {
                token.cancel();
            }
            if path.ends_with("-manifest.csv") {
                let bytes = if matches!(mode, Mode::BadManifest) {
                    "s3://other-synthetic/private/file\n".to_owned()
                } else if matches!(mode, Mode::NoFiles) {
                    String::new()
                } else {
                    (0..state.files.len())
                        .map(|index| format!("{}part-{index}.parquet\n", state.prefix))
                        .collect::<String>()
                };
                (
                    200,
                    bytes.into_bytes(),
                    !matches!(mode, Mode::BadEncryption),
                )
            } else if matches!(mode, Mode::MissingFile) {
                (
                    404,
                    b"<Error><Code>NoSuchKey</Code><Message>synthetic detail</Message></Error>"
                        .to_vec(),
                    true,
                )
            } else {
                let part: usize = path
                    .rsplit("part-")
                    .next()
                    .unwrap()
                    .split('.')
                    .next()
                    .unwrap()
                    .parse()
                    .unwrap();
                (200, state.files[part].clone(), true)
            }
        };
        let mut response = http::Response::builder()
            .status(status)
            .header("content-length", body.len());
        if encrypted {
            response = match encryption {
                Encryption::SseS3 => response.header("x-amz-server-side-encryption", "AES256"),
                Encryption::Kms { actual, .. } => {
                    let response = response.header("x-amz-server-side-encryption", "aws:kms");
                    if let Some(key) = actual {
                        response.header("x-amz-server-side-encryption-aws-kms-key-id", key)
                    } else {
                        response
                    }
                }
            };
        }
        response.body(body).unwrap()
    });
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
        .time_source(StaticTimeSource::from_secs(1_704_067_200))
        .retry_config(RetryConfig::standard().with_max_attempts(1))
        .http_client(http)
        .build();
    let mut options = ConnectionOptions::new("synthetic", "schema");
    if matches!(mode, Mode::Expiring) {
        options.role_arn = Some("arn:aws:iam::000000000000:role/synthetic".into());
    }
    let connection = Connection::from_config(options, config).await.unwrap();
    (connection, state)
}

#[tokio::test]
async fn successful_query_surfaces_expiry_and_reader_conversion_retains_warnings() {
    let (connection, _) = service(Mode::Expiring, None).await;
    let result = connection
        .query(SQL, &QueryOptions::default(), &QueryCancellation::new())
        .await
        .unwrap();
    assert!(
        matches!(result.warnings(),[QueryWarning::SessionExpiring{remaining}] if *remaining<=std::time::Duration::from_secs(600))
    );
    let (reader, warnings) = result.into_reader_with_warnings();
    assert!(matches!(
        warnings.as_slice(),
        [QueryWarning::SessionExpiring { .. }]
    ));
    assert_eq!(
        reader.map(|batch| batch.unwrap().num_rows()).sum::<usize>(),
        4
    );
}

#[tokio::test(start_paused = true)]
async fn worker_wait_obeys_deadline_and_cancellation_while_worker_is_pending() {
    use std::time::Duration;
    let (connection, _) = service(Mode::Rows, None).await;
    let token = QueryCancellation::new();
    let start = Instant::now();
    let guard = RequestGuard {
        connection: &connection,
        cancellation: &token,
        deadline: start + Duration::from_secs(2),
    };
    let task = tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(10)).await;
        Ok::<_, ResultError>(())
    });
    assert!(matches!(
        wait_for_worker(&guard, task, "test").await,
        Err(ResultError::Interrupted(QueryInterruption::TimedOut))
    ));
    assert_eq!(Instant::now() - start, Duration::from_secs(2));
    let guard = RequestGuard {
        connection: &connection,
        cancellation: &token,
        deadline: Instant::now() + Duration::from_secs(30),
    };
    let task = tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(10)).await;
        Ok::<_, ResultError>(())
    });
    let (result, ()) = tokio::join!(wait_for_worker(&guard, task, "test"), async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        token.cancel();
    });
    assert!(matches!(
        result,
        Err(ResultError::Interrupted(QueryInterruption::Cancelled))
    ));
}

#[tokio::test]
async fn public_query_preserves_arrow_values_and_globally_sorts_multiple_files() {
    let (connection, state) = service(Mode::Rows, None).await;
    let result = connection
        .query(SQL, &QueryOptions::default(), &QueryCancellation::new())
        .await
        .unwrap();
    assert_eq!(result.row_count(), 4);
    assert!(result.warnings().is_empty());
    let expected_schema = Schema::new(vec![
        Field::new("label", DataType::Utf8, true),
        Field::new("n", DataType::Int64, true),
        Field::new("s_null", DataType::Utf8, true),
        Field::new("s_empty", DataType::Utf8, true),
        Field::new(
            "ts",
            DataType::Timestamp(arrow_schema::TimeUnit::Millisecond, None),
            true,
        ),
        Field::new("dec", DataType::Decimal128(10, 2), true),
        Field::new(
            "arr",
            DataType::List(Arc::new(Field::new("item", DataType::Int64, true))),
            true,
        ),
    ]);
    assert_eq!(result.schema().as_ref(), &expected_schema);
    let batches: Vec<_> = result.into_reader().collect::<Result<_, _>>().unwrap();
    let batch = arrow_select::concat::concat_batches(&Arc::new(expected_schema), &batches).unwrap();
    let n = batch
        .column(1)
        .as_any()
        .downcast_ref::<Int64Array>()
        .unwrap();
    assert_eq!(
        n.iter().collect::<Vec<_>>(),
        vec![Some(3), Some(1), Some(9_007_199_254_740_993), Some(9)]
    );
    let labels = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(
        labels.iter().collect::<Vec<_>>(),
        vec![Some("a"), Some("a"), Some("b"), None]
    );
    let nulls = batch
        .column(2)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(
        nulls.iter().collect::<Vec<_>>(),
        vec![None, Some("a"), None, Some("a")]
    );
    let empty = batch
        .column(3)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(
        empty.iter().collect::<Vec<_>>(),
        vec![Some(""), None, Some(""), None]
    );
    let ts = batch
        .column(4)
        .as_any()
        .downcast_ref::<PrimitiveArray<TimestampMillisecondType>>()
        .unwrap();
    assert_eq!(
        ts.iter().collect::<Vec<_>>(),
        vec![Some(1_792_154_096_789), None, Some(1_792_154_096_789), None]
    );
    let decimals = batch
        .column(5)
        .as_any()
        .downcast_ref::<Decimal128Array>()
        .unwrap();
    assert_eq!(
        decimals.iter().collect::<Vec<_>>(),
        vec![Some(110), None, Some(110), None]
    );
    let lists = batch
        .column(6)
        .as_any()
        .downcast_ref::<ListArray>()
        .unwrap();
    let lists: Vec<Option<Vec<Option<i64>>>> = lists
        .iter()
        .map(|list| {
            list.map(|list| {
                list.as_any()
                    .downcast_ref::<Int64Array>()
                    .unwrap()
                    .iter()
                    .collect()
            })
        })
        .collect();
    assert_eq!(
        lists,
        vec![
            Some(vec![Some(1), None]),
            None,
            Some(vec![Some(1), None]),
            None
        ]
    );
    let state = state.lock().unwrap();
    assert_eq!(
        state
            .requests
            .iter()
            .filter(|op| op.starts_with("GET "))
            .count(),
        3
    );
    assert!(
        state
            .requests
            .iter()
            .all(|op| !op.contains("List") && !op.contains("Delete"))
    );
}

#[tokio::test]
async fn empty_result_retains_nested_schema_and_all_null_real_rows_survive() {
    for (mode, expected_rows) in [(Mode::Empty, 0), (Mode::AllNull, 2)] {
        let (connection, _) = service(mode, None).await;
        let result = connection
            .query(SQL, &QueryOptions::default(), &QueryCancellation::new())
            .await
            .unwrap();
        assert_eq!(result.row_count(), expected_rows);
        assert_eq!(result.schema().field(1).data_type(), &DataType::Int64);
        assert!(matches!(
            result.schema().field(6).data_type(),
            DataType::List(_)
        ));
        assert!(
            !result
                .schema()
                .fields()
                .iter()
                .any(|field| field.name().starts_with("__ckdata_presence_"))
        );
    }
}

#[tokio::test]
async fn invalid_results_fail_without_partial_data_and_retain_modeled_service_guidance() {
    for mode in [
        Mode::BadManifest,
        Mode::BadEncryption,
        Mode::MissingFile,
        Mode::MismatchedSchema,
        Mode::Timezone,
        Mode::BadPresence,
        Mode::NoFiles,
    ] {
        let (connection, _) = service(mode, None).await;
        let error = connection
            .query(SQL, &QueryOptions::default(), &QueryCancellation::new())
            .await
            .unwrap_err();
        assert!(matches!(&error, Error::Result { .. }));
        if matches!(mode, Mode::MissingFile) {
            assert!(
                matches!(&error, Error::Result { reason: ResultError::Interrupted(QueryInterruption::RequestFailed { code, message, .. }), .. } if code == "NoSuchKey" && message.as_deref() == Some("synthetic detail"))
            );
            assert!(error.to_string().contains("synthetic detail"));
        }
    }
}

#[tokio::test]
async fn result_limits_reject_oversized_reads_and_validate_before_network() {
    for limits in [
        ResultLimits {
            max_files: 1,
            ..ResultLimits::default()
        },
        ResultLimits {
            max_manifest_bytes: 8,
            ..ResultLimits::default()
        },
        ResultLimits {
            max_file_bytes: 8,
            ..ResultLimits::default()
        },
        ResultLimits {
            max_download_bytes: 8,
            ..ResultLimits::default()
        },
        ResultLimits {
            max_decoded_bytes: 8,
            ..ResultLimits::default()
        },
        ResultLimits {
            max_rows: 2,
            ..ResultLimits::default()
        },
    ] {
        let (connection, _) = service(Mode::Rows, None).await;
        let options = QueryOptions {
            result_limits: limits,
            ..QueryOptions::default()
        };
        assert!(matches!(
            connection
                .query(SQL, &options, &QueryCancellation::new())
                .await,
            Err(Error::Result {
                reason: ResultError::LimitExceeded(_),
                ..
            })
        ));
    }
    let (connection, state) = service(Mode::Rows, None).await;
    let options = QueryOptions {
        result_limits: ResultLimits {
            download_concurrency: 0,
            ..ResultLimits::default()
        },
        ..QueryOptions::default()
    };
    assert!(matches!(
        connection
            .query(SQL, &options, &QueryCancellation::new())
            .await,
        Err(Error::InvalidSetting(_))
    ));
    assert!(state.lock().unwrap().requests.is_empty());
}

#[tokio::test]
async fn cancellation_during_result_transfer_is_typed_and_does_not_stop_completed_query() {
    let token = QueryCancellation::new();
    let (connection, state) = service(Mode::Rows, Some(token.clone())).await;
    let error = connection
        .query(SQL, &QueryOptions::default(), &token)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::Result {
            reason: ResultError::Interrupted(QueryInterruption::Cancelled),
            ..
        }
    ));
    assert!(
        !state
            .lock()
            .unwrap()
            .requests
            .iter()
            .any(|op| op == "StopQueryExecution")
    );
}

#[tokio::test]
async fn hidden_order_key_returns_an_explicit_warning() {
    let (connection, _) = service(Mode::Rows, None).await;
    let result = connection
        .query(
            "SELECT * FROM synthetic ORDER BY hidden",
            &QueryOptions::default(),
            &QueryCancellation::new(),
        )
        .await
        .unwrap();
    assert!(matches!(
        result.warnings(),
        [QueryWarning::OrderNotRestored { .. }]
    ));
    assert_eq!(result.row_count(), 4);
}

#[test]
fn parquet_result_fixtures_use_snappy() {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    let bytes = parquet(&batch("synthetic_presence", 0, Mode::Rows));
    let reader = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes)).unwrap();
    assert!(
        reader
            .metadata()
            .row_groups()
            .iter()
            .flat_map(|group| group.columns())
            .all(|column| column.compression() == Compression::SNAPPY)
    );
}

#[tokio::test]
async fn public_query_restores_explicit_and_default_null_placement_across_files() {
    for (order, expected) in [
        (
            "ASC NULLS FIRST",
            vec![
                (None, 9),
                (Some("a"), 3),
                (Some("a"), 1),
                (Some("b"), 9_007_199_254_740_993),
            ],
        ),
        (
            "DESC NULLS FIRST",
            vec![
                (None, 9),
                (Some("b"), 9_007_199_254_740_993),
                (Some("a"), 3),
                (Some("a"), 1),
            ],
        ),
        (
            "ASC",
            vec![
                (Some("a"), 3),
                (Some("a"), 1),
                (Some("b"), 9_007_199_254_740_993),
                (None, 9),
            ],
        ),
        (
            "DESC",
            vec![
                (Some("b"), 9_007_199_254_740_993),
                (Some("a"), 3),
                (Some("a"), 1),
                (None, 9),
            ],
        ),
    ] {
        let (connection, _) = service(Mode::Rows, None).await;
        let result = connection
            .query(
                &format!("SELECT label, n FROM synthetic ORDER BY label {order}, n DESC"),
                &QueryOptions::default(),
                &QueryCancellation::new(),
            )
            .await
            .unwrap();
        assert!(result.warnings().is_empty());
        let actual: Vec<_> = result
            .batches()
            .iter()
            .flat_map(|batch| {
                let labels = batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap();
                let ids = batch
                    .column(1)
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .unwrap();
                labels
                    .iter()
                    .zip(ids.iter())
                    .map(|(label, id)| (label.map(str::to_owned), id))
                    .collect::<Vec<_>>()
            })
            .collect();
        let expected: Vec<_> = expected
            .into_iter()
            .map(|(label, id)| (label.map(str::to_owned), Some(id)))
            .collect();
        assert_eq!(actual, expected, "ORDER BY label {order}");
    }
}

#[tokio::test]
async fn public_query_checks_kms_key_identity_for_arns_and_uuids() {
    for (configured, actual, accepted) in [
        (KEY_ARN, Some(KEY_ARN), true),
        (KEY_UUID, Some(KEY_ARN), true),
        (KEY_UUID, Some(KEY_UUID), true),
        (KEY_ARN, Some(OTHER_KEY), false),
        (KEY_UUID, Some(OTHER_KEY), false),
        (KEY_ARN, Some(OTHER_ACCOUNT_KEY), false),
        (KEY_ARN, Some(KEY_UUID), false),
        (KEY_ARN, None, false),
        (KEY_UUID, None, false),
    ] {
        let (connection, _) = service_with_encryption(
            Mode::Rows,
            None,
            Encryption::Kms {
                configured: Some(configured),
                actual,
            },
        )
        .await;
        let result = connection
            .query(SQL, &QueryOptions::default(), &QueryCancellation::new())
            .await;
        if accepted {
            assert_eq!(result.unwrap().row_count(), 4);
        } else {
            assert!(
                matches!(
                    result,
                    Err(Error::Result {
                        reason: ResultError::EncryptionMismatch,
                        ..
                    })
                ),
                "configured {configured}, actual {actual:?}"
            );
        }
    }
}

#[tokio::test]
async fn public_query_rejects_unverifiable_kms_settings_before_submission() {
    for configured in [
        None,
        Some("alias/synthetic"),
        Some("arn:aws:kms:us-west-2:000000000000:alias/synthetic"),
        Some("not-a-key"),
    ] {
        let (connection, state) = service_with_encryption(
            Mode::Rows,
            None,
            Encryption::Kms {
                configured,
                actual: Some(KEY_ARN),
            },
        )
        .await;
        assert!(matches!(
            connection
                .query(SQL, &QueryOptions::default(), &QueryCancellation::new())
                .await,
            Err(Error::InvalidSetting(_))
        ));
        assert_eq!(state.lock().unwrap().requests, ["GetWorkGroup"]);
    }
}

#[tokio::test]
async fn decoded_array_budget_is_independent_of_parquet_preflight_and_accepts_exact_limit() {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    let (connection, state) = service(Mode::CompactRows, None).await;
    let result = connection
        .query(
            "SELECT label FROM synthetic",
            &QueryOptions::default(),
            &QueryCancellation::new(),
        )
        .await
        .unwrap();
    let decoded: usize = result
        .batches()
        .iter()
        .map(RecordBatch::get_array_memory_size)
        .sum();
    let file = state.lock().unwrap().files[0].clone();
    let reader = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(file)).unwrap();
    let uncompressed: usize = reader
        .metadata()
        .row_groups()
        .iter()
        .map(|group| usize::try_from(group.total_byte_size()).unwrap())
        .sum();
    assert!(
        decoded > uncompressed,
        "fixture must pass the earlier uncompressed-byte preflight"
    );
    for (limit, accepted) in [(decoded - 1, false), (decoded, true)] {
        let (connection, _) = service(Mode::CompactRows, None).await;
        let options = QueryOptions {
            result_limits: ResultLimits {
                max_decoded_bytes: limit,
                ..ResultLimits::default()
            },
            ..QueryOptions::default()
        };
        let result = connection
            .query(
                "SELECT label FROM synthetic",
                &options,
                &QueryCancellation::new(),
            )
            .await;
        if accepted {
            assert_eq!(result.unwrap().row_count(), 128);
        } else {
            assert!(matches!(
                result,
                Err(Error::Result {
                    reason: ResultError::LimitExceeded("decoded bytes"),
                    ..
                })
            ));
        }
    }
}

#[tokio::test]
async fn real_row_limit_rejects_the_preflight_allowance_and_accepts_exact_limit() {
    for (max_rows, accepted) in [(3, false), (4, true)] {
        let (connection, _) = service(Mode::Rows, None).await;
        let options = QueryOptions {
            result_limits: ResultLimits {
                max_rows,
                ..ResultLimits::default()
            },
            ..QueryOptions::default()
        };
        let result = connection
            .query(SQL, &options, &QueryCancellation::new())
            .await;
        if accepted {
            assert_eq!(result.unwrap().row_count(), 4);
        } else {
            assert!(matches!(
                result,
                Err(Error::Result {
                    reason: ResultError::LimitExceeded("row count"),
                    ..
                })
            ));
        }
    }
}
