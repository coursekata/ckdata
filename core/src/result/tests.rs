use super::*;
use std::sync::{Arc, Mutex};

use arrow_array::types::{Int64Type, TimestampMillisecondType};
use arrow_array::{
    Array, ArrayRef, Decimal128Array, Int64Array, ListArray, PrimitiveArray, StringArray,
    new_null_array,
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
    TraversalManifest,
    EncodedTraversalManifest,
    BadEncryption,
    MissingFile,
    MismatchedSchema,
    Timezone,
    NoFiles,
    NoManifest,
    Headers,
    DuplicateFile,
    ManifestFailure,
    MissingResultSet,
    InvalidParquet,
    Expiring,
    ExpiringNoFiles,
    NestedNoFiles,
    NoFilesAgain,
    RecoveryRows,
    RecoveryFailed,
    RecoveryBadManifest,
    RecoveryBadEncryption,
    RecoveryMissingFile,
    RecoveryInvalidParquet,
    RecoveryMismatchedSchema,
    CancelBeforeRecovery,
    CancelDuringRecovery,
    RecoverySlow,
    QueryFailed,
}

impl Mode {
    fn fileless(self) -> bool {
        matches!(
            self,
            Self::NoFiles
                | Self::ExpiringNoFiles
                | Self::NestedNoFiles
                | Self::NoFilesAgain
                | Self::RecoveryRows
                | Self::RecoveryFailed
                | Self::RecoveryBadManifest
                | Self::RecoveryBadEncryption
                | Self::RecoveryMissingFile
                | Self::RecoveryInvalidParquet
                | Self::RecoveryMismatchedSchema
                | Self::CancelBeforeRecovery
                | Self::CancelDuringRecovery
                | Self::RecoverySlow
        )
    }
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
    submitted_sql: String,
    submissions: Vec<Value>,
    polls: usize,
}

fn nested_batch() -> RecordBatch {
    let details = DataType::Struct(
        vec![
            Field::new("id", DataType::Int64, false),
            Field::new(
                "amounts",
                DataType::List(Arc::new(Field::new(
                    "item",
                    DataType::Decimal128(38, 18),
                    true,
                ))),
                true,
            ),
        ]
        .into(),
    );
    let mapping = DataType::Map(
        Arc::new(Field::new(
            "entries",
            DataType::Struct(
                vec![
                    Field::new("key", DataType::Utf8, false),
                    Field::new("value", details.clone(), true),
                ]
                .into(),
            ),
            false,
        )),
        false,
    );
    let schema = Arc::new(Schema::new(vec![
        Field::new("details", details, true),
        Field::new("mapping", mapping, true),
    ]));
    let columns = schema
        .fields()
        .iter()
        .map(|field| new_null_array(field.data_type(), 1))
        .collect();
    RecordBatch::try_new(schema, columns).unwrap()
}

fn nested_parquet() -> Vec<u8> {
    use parquet::arrow::arrow_writer::ArrowWriterOptions;
    let batch = nested_batch();
    let mut bytes = Vec::new();
    let options = ArrowWriterOptions::new().with_skip_arrow_metadata(true);
    let mut writer =
        ArrowWriter::try_new_with_options(&mut bytes, batch.schema(), options).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
    bytes
}

fn batch(part: usize, mode: Mode) -> RecordBatch {
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
    ]));
    if matches!(mode, Mode::Empty | Mode::AllNull) {
        let rows = if matches!(mode, Mode::Empty) { 0 } else { 2 };
        let columns = schema
            .fields()
            .iter()
            .map(|field| new_null_array(field.data_type(), rows))
            .collect();
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
            assert!(matches!(mode, Mode::Expiring | Mode::ExpiringNoFiles));
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
                    state.submitted_sql = sql.to_owned();
                    state.submissions.push(body.clone());
                    state.polls = 0;
                    state.prefix = sql
                        .split(" TO '")
                        .nth(1)
                        .unwrap()
                        .split('\'')
                        .next()
                        .unwrap()
                        .to_owned();
                    let recovery = state.submissions.len() > 1;
                    state.files = vec![parquet(&batch(0, mode))];
                    if !matches!(mode, Mode::Empty | Mode::AllNull) {
                        state.files.push(parquet(&batch(1, mode)));
                    }
                    if recovery && !matches!(mode, Mode::RecoveryRows) {
                        state.files = vec![parquet(&batch(0, Mode::AllNull).slice(0, 1))];
                    }
                    if recovery && matches!(mode, Mode::NestedNoFiles) {
                        state.files = vec![nested_parquet()];
                    }
                    if recovery && matches!(mode, Mode::RecoveryMismatchedSchema) {
                        state.files.push(parquet(&batch(1, Mode::MismatchedSchema)));
                    }
                    if matches!(mode, Mode::InvalidParquet)
                        || (recovery && matches!(mode, Mode::RecoveryInvalidParquet))
                    {
                        state.files = vec![b"not a parquet file".to_vec()];
                    }
                    json!({"QueryExecutionId": if recovery { "synthetic-recovery" } else { "synthetic-query" }})
                }
                "GetQueryExecution" => {
                    state.polls += 1;
                    let recovery = state.submissions.len() > 1;
                    let query_id = if recovery {
                        "synthetic-recovery"
                    } else {
                        "synthetic-query"
                    };
                    let mut actual = configuration;
                    actual["OutputLocation"] = json!(format!("{OUTPUT}synthetic-query.txt"));
                    let mut value = json!({"QueryExecution": {"QueryExecutionId": query_id, "WorkGroup": "synthetic", "Status": {"State": "SUCCEEDED"}, "Statistics": {"DataManifestLocation": format!("{OUTPUT}{query_id}-manifest.csv")}, "ResultConfiguration": actual}});
                    if matches!(mode, Mode::QueryFailed)
                        || (recovery && matches!(mode, Mode::RecoveryFailed))
                    {
                        value["QueryExecution"]["Status"] = json!({"State": "FAILED", "StateChangeReason": "synthetic failure detail"});
                    }
                    if matches!(mode, Mode::RecoverySlow) && state.polls == 1 {
                        value["QueryExecution"]["Status"] = json!({"State": "RUNNING"});
                    }
                    if recovery && matches!(mode, Mode::CancelDuringRecovery) {
                        value["QueryExecution"]["Status"] = json!({"State": "RUNNING"});
                        token.as_ref().unwrap().cancel();
                    }
                    if matches!(mode, Mode::NoManifest) {
                        value["QueryExecution"]["Statistics"] = json!({});
                    }
                    value
                }
                "GetQueryResults" => {
                    let body: Value =
                        serde_json::from_slice(request.body().bytes().unwrap()).unwrap();
                    let recovery = state.submissions.len() > 1;
                    assert_eq!(
                        body["QueryExecutionId"],
                        if recovery {
                            "synthetic-recovery"
                        } else {
                            "synthetic-query"
                        }
                    );
                    assert_eq!(body["QueryResultType"], "DATA_MANIFEST");
                    if matches!(mode, Mode::MissingResultSet) {
                        return http::Response::builder()
                            .status(200)
                            .header("content-type", "application/x-amz-json-1.1")
                            .body(b"{}".to_vec())
                            .unwrap();
                    }
                    if matches!(mode, Mode::ManifestFailure) {
                        return http::Response::builder()
                            .status(400)
                            .header("content-type", "application/x-amz-json-1.1")
                            .body(br#"{"__type":"InvalidRequestException","Message":"synthetic manifest detail"}"#.to_vec())
                            .unwrap();
                    }
                    if matches!(mode, Mode::CancelBeforeRecovery) && !recovery {
                        token.as_ref().unwrap().cancel();
                    }
                    if (mode.fileless() && !recovery) || matches!(mode, Mode::NoFilesAgain) {
                        json!({"ResultSet": {}})
                    } else {
                        let part = if body["NextToken"].as_str() == Some("second") {
                            1
                        } else {
                            0
                        };
                        let mut rows = Vec::new();
                        if matches!(mode, Mode::Headers) {
                            rows.push(json!({"Data": [{"VarCharValue": "location"}]}));
                        }
                        let uri = if matches!(mode, Mode::BadManifest)
                            || (recovery && matches!(mode, Mode::RecoveryBadManifest))
                        {
                            "s3://other-synthetic/private/file".to_owned()
                        } else if matches!(mode, Mode::TraversalManifest) {
                            format!("{}../part.parquet", state.prefix)
                        } else if matches!(mode, Mode::EncodedTraversalManifest) {
                            format!("{}%2e%2e/part.parquet", state.prefix)
                        } else {
                            let file = if matches!(mode, Mode::DuplicateFile) {
                                0
                            } else {
                                part
                            };
                            format!("{}part-{file}.parquet", state.prefix)
                        };
                        rows.push(json!({"Data": [{"VarCharValue": uri}]}));
                        let mut response = json!({"ResultSet": {
                            "ResultSetMetadata": {"ColumnInfo": [{"Name": "location", "Type": "varchar"}]},
                            "Rows": rows
                        }});
                        if state.files.len() > 1 && part == 0 {
                            response["NextToken"] = json!("second");
                        }
                        response
                    }
                }
                "StopQueryExecution" => {
                    let body: Value =
                        serde_json::from_slice(request.body().bytes().unwrap()).unwrap();
                    assert_eq!(body["QueryExecutionId"], "synthetic-recovery");
                    json!({})
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
            if matches!(mode, Mode::MissingFile | Mode::RecoveryMissingFile) {
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
                (
                    200,
                    state.files[part].clone(),
                    !matches!(mode, Mode::BadEncryption | Mode::RecoveryBadEncryption),
                )
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
    if matches!(mode, Mode::Expiring | Mode::ExpiringNoFiles) {
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
        wait_for_worker(&guard, task).await,
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
    let (result, ()) = tokio::join!(wait_for_worker(&guard, task), async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        token.cancel();
    });
    assert!(matches!(
        result,
        Err(ResultError::Interrupted(QueryInterruption::Cancelled))
    ));
}

#[tokio::test]
async fn public_query_preserves_values_without_rewriting_sql_or_sorting_rows() {
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
        vec![Some(9_007_199_254_740_993), Some(9), Some(3), Some(1)]
    );
    let labels = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(
        labels.iter().collect::<Vec<_>>(),
        vec![Some("b"), None, Some("a"), Some("a")]
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
        state.submitted_sql,
        format!(
            "UNLOAD (\n{SQL}\n) TO '{}' WITH (format = 'PARQUET', compression = 'SNAPPY')",
            state.prefix
        )
    );
    assert_eq!(
        state
            .requests
            .iter()
            .filter(|op| *op == "GetQueryResults")
            .count(),
        2
    );
    assert_eq!(
        state
            .requests
            .iter()
            .filter(|op| op.starts_with("GET "))
            .count(),
        2
    );
    assert!(
        state
            .requests
            .iter()
            .all(|op| !op.contains("List") && !op.contains("Delete"))
    );
}

#[tokio::test]
async fn fileless_empty_results_recover_schema_without_returning_probe_rows() {
    for mode in [Mode::NoFiles, Mode::ExpiringNoFiles, Mode::RecoveryRows] {
        let (connection, state) = service(mode, None).await;
        let result = connection
            .query(SQL, &QueryOptions::default(), &QueryCancellation::new())
            .await
            .unwrap();
        assert_eq!(result.row_count(), 0);
        assert_eq!(result.query_id(), "synthetic-query");
        assert_eq!(result.schema().fields().len(), 7);
        assert!(matches!(
            result.schema().field(6).data_type(),
            DataType::List(_)
        ));
        assert!(result.batches().is_empty());
        let (reader, warnings) = result.into_reader_with_warnings();
        assert_eq!(reader.schema().fields().len(), 7);
        assert_eq!(reader.count(), 0);
        if matches!(mode, Mode::ExpiringNoFiles) {
            assert!(matches!(
                warnings.as_slice(),
                [QueryWarning::SessionExpiring { .. }]
            ));
        }
        let state = state.lock().unwrap();
        assert_eq!(
            state
                .requests
                .iter()
                .filter(|op| *op == "StartQueryExecution")
                .count(),
            2
        );
        assert_ne!(
            state.submissions[0]["ClientRequestToken"],
            state.submissions[1]["ClientRequestToken"]
        );
        let original = state.submissions[0]["QueryString"].as_str().unwrap();
        let recovery = state.submissions[1]["QueryString"].as_str().unwrap();
        assert!(original.starts_with(&format!("UNLOAD (\n{SQL}\n)")));
        assert!(recovery.contains(&format!("(\n{SQL}\n) AS original_rows LIMIT 0")));
        assert!(recovery.contains("SELECT schema_rows.*"));
        assert!(
            state
                .requests
                .iter()
                .all(|op| !op.contains("List") && !op.contains("Delete"))
        );
    }
}

#[tokio::test]
async fn recovery_reads_nested_schema_from_physical_parquet_without_arrow_metadata() {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    let expected = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(nested_parquet()))
        .unwrap()
        .schema()
        .clone();
    let (connection, _) = service(Mode::NestedNoFiles, None).await;
    let result = connection
        .query(SQL, &QueryOptions::default(), &QueryCancellation::new())
        .await
        .unwrap();
    assert_eq!(result.schema(), expected);
    assert_eq!(result.row_count(), 0);
    assert!(matches!(
        result.schema().field(0).data_type(),
        DataType::Struct(_)
    ));
    assert!(matches!(
        result.schema().field(1).data_type(),
        DataType::Map(_, _)
    ));
    assert_eq!(result.into_reader().count(), 0);
}

#[tokio::test]
async fn recovery_failures_retain_original_and_recovery_ids_without_recursing() {
    for mode in [
        Mode::NoFilesAgain,
        Mode::RecoveryFailed,
        Mode::RecoveryBadManifest,
        Mode::RecoveryBadEncryption,
        Mode::RecoveryMissingFile,
        Mode::RecoveryInvalidParquet,
        Mode::RecoveryMismatchedSchema,
    ] {
        let (connection, state) = service(mode, None).await;
        let error = connection
            .query(SQL, &QueryOptions::default(), &QueryCancellation::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("synthetic-query"));
        assert!(error.to_string().contains("synthetic-recovery"));
        let Error::SchemaRecovery { query_id, source } = error else {
            panic!("missing recovery context")
        };
        assert_eq!(query_id, "synthetic-query");
        match *source {
            Error::QueryFailed { query_id, reason } => {
                assert!(matches!(mode, Mode::RecoveryFailed));
                assert_eq!(query_id, "synthetic-recovery");
                assert_eq!(reason, "synthetic failure detail");
            }
            Error::Result { query_id, reason } => {
                assert_eq!(query_id, "synthetic-recovery");
                match mode {
                    Mode::NoFilesAgain | Mode::RecoveryMismatchedSchema => {
                        assert!(matches!(reason, ResultError::InvalidSchema(_)))
                    }
                    Mode::RecoveryBadManifest => {
                        assert!(matches!(reason, ResultError::InvalidManifest(_)))
                    }
                    Mode::RecoveryBadEncryption => {
                        assert_eq!(reason, ResultError::EncryptionMismatch)
                    }
                    Mode::RecoveryInvalidParquet => {
                        assert!(matches!(reason, ResultError::InvalidParquet(_)))
                    }
                    Mode::RecoveryMissingFile => assert!(
                        matches!(reason, ResultError::Interrupted(QueryInterruption::RequestFailed { ref code, .. }) if code == "NoSuchKey")
                    ),
                    _ => panic!("unexpected recovery error"),
                }
            }
            other => panic!("unexpected recovery error: {other}"),
        }
        let state = state.lock().unwrap();
        assert_eq!(state.submissions.len(), 2);
        if matches!(mode, Mode::NoFilesAgain | Mode::RecoveryBadManifest) {
            assert!(!state.requests.iter().any(|op| op.starts_with("GET ")));
        }
    }
}

#[tokio::test]
async fn cancellation_after_empty_execution_prevents_recovery_submission() {
    let token = QueryCancellation::new();
    let (connection, state) = service(Mode::CancelBeforeRecovery, Some(token.clone())).await;
    let error = connection
        .query(SQL, &QueryOptions::default(), &token)
        .await
        .unwrap_err();
    // Cancellation may be observed while the last manifest page is finishing
    // or by the recovery's pre-submission guard; neither may start a query.
    assert!(
        matches!(
            &error,
            Error::Result {
                reason: ResultError::Interrupted(QueryInterruption::Cancelled),
                ..
            }
        ) || matches!(&error, Error::SchemaRecovery { source, .. } if matches!(source.as_ref(), Error::QueryInterrupted { query_id: None, reason: QueryInterruption::Cancelled, .. })),
        "{error:?}"
    );
    let state = state.lock().unwrap();
    assert_eq!(state.submissions.len(), 1);
    assert!(!state.requests.iter().any(|op| op == "StopQueryExecution"));
}

#[tokio::test]
async fn cancellation_stops_only_the_active_schema_recovery_execution() {
    let token = QueryCancellation::new();
    let (connection, state) = service(Mode::CancelDuringRecovery, Some(token.clone())).await;
    let error = connection
        .query(SQL, &QueryOptions::default(), &token)
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::SchemaRecovery { source, .. } if matches!(*source, Error::QueryInterrupted { query_id: Some(ref id), reason: QueryInterruption::Cancelled, cancellation: crate::CancellationStatus::Requested } if id == "synthetic-recovery"))
    );
    let state = state.lock().unwrap();
    assert_eq!(state.submissions.len(), 2);
    assert_eq!(
        state
            .requests
            .iter()
            .filter(|op| *op == "StopQueryExecution")
            .count(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn recovery_shares_the_original_deadline_instead_of_resetting_it() {
    use std::time::Duration;
    let (connection, state) = service(Mode::RecoverySlow, None).await;
    let options = QueryOptions {
        timeout: Duration::from_secs(2),
        poll_interval: Duration::from_secs(1),
    };
    let start = Instant::now();
    let error = connection
        .query(SQL, &options, &QueryCancellation::new())
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::SchemaRecovery { source, .. } if matches!(*source, Error::QueryInterrupted { query_id: Some(ref id), reason: QueryInterruption::TimedOut, cancellation: crate::CancellationStatus::Requested } if id == "synthetic-recovery"))
    );
    assert_eq!(Instant::now() - start, Duration::from_secs(2));
    assert_eq!(state.lock().unwrap().submissions.len(), 2);
}

#[tokio::test]
async fn failed_original_query_does_not_attempt_schema_recovery() {
    let (connection, state) = service(Mode::QueryFailed, None).await;
    let error = connection
        .query(SQL, &QueryOptions::default(), &QueryCancellation::new())
        .await
        .unwrap_err();
    assert!(matches!(error, Error::QueryFailed { .. }));
    assert_eq!(state.lock().unwrap().submissions.len(), 1);
}

#[tokio::test]
async fn real_empty_parquet_retains_schema_and_all_null_rows_survive() {
    for (mode, expected_rows) in [(Mode::Empty, 0), (Mode::AllNull, 2)] {
        let (connection, state) = service(mode, None).await;
        let result = connection
            .query(SQL, &QueryOptions::default(), &QueryCancellation::new())
            .await
            .unwrap();
        assert_eq!(result.row_count(), expected_rows);
        let schema = result.schema();
        assert_eq!(schema.fields().len(), 7);
        assert_eq!(schema.field(1).data_type(), &DataType::Int64);
        assert!(matches!(schema.field(6).data_type(), DataType::List(_)));
        assert_eq!(
            result
                .into_reader()
                .map(|batch| batch.unwrap().num_rows())
                .sum::<usize>(),
            expected_rows
        );
        assert_eq!(state.lock().unwrap().submissions.len(), 1);
    }
}

#[tokio::test]
async fn manifest_headers_are_optional_and_pagination_keeps_every_file() {
    let (connection, state) = service(Mode::Headers, None).await;
    let result = connection
        .query(SQL, &QueryOptions::default(), &QueryCancellation::new())
        .await
        .unwrap();
    assert_eq!(result.row_count(), 4);
    assert_eq!(
        state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|op| *op == "GetQueryResults")
            .count(),
        2
    );
}

#[tokio::test]
async fn timezone_metadata_is_preserved_by_the_parquet_reader() {
    let (connection, _) = service(Mode::Timezone, None).await;
    let result = connection
        .query(SQL, &QueryOptions::default(), &QueryCancellation::new())
        .await
        .unwrap();
    assert_eq!(
        result.schema().field(4).data_type(),
        &DataType::Timestamp(arrow_schema::TimeUnit::Millisecond, Some("UTC".into()))
    );
}

#[tokio::test]
async fn invalid_results_fail_without_partial_data_and_retain_modeled_service_guidance() {
    for mode in [
        Mode::BadManifest,
        Mode::TraversalManifest,
        Mode::EncodedTraversalManifest,
        Mode::BadEncryption,
        Mode::MissingFile,
        Mode::MismatchedSchema,
        Mode::DuplicateFile,
        Mode::ManifestFailure,
        Mode::MissingResultSet,
        Mode::InvalidParquet,
    ] {
        let (connection, state) = service(mode, None).await;
        let error = connection
            .query(SQL, &QueryOptions::default(), &QueryCancellation::new())
            .await
            .unwrap_err();
        assert!(matches!(&error, Error::Result { .. }));
        assert_eq!(state.lock().unwrap().submissions.len(), 1);
        if matches!(
            mode,
            Mode::BadManifest
                | Mode::TraversalManifest
                | Mode::EncodedTraversalManifest
                | Mode::DuplicateFile
        ) {
            assert!(
                !state
                    .lock()
                    .unwrap()
                    .requests
                    .iter()
                    .any(|op| op.starts_with("GET "))
            );
        }
        if matches!(mode, Mode::MissingFile) {
            assert!(
                matches!(&error, Error::Result { reason: ResultError::Interrupted(QueryInterruption::RequestFailed { code, message, .. }), .. } if code == "NoSuchKey" && message.as_deref() == Some("synthetic detail"))
            );
            assert!(error.to_string().contains("synthetic detail"));
        }
    }
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

#[test]
fn parquet_result_fixtures_use_snappy() {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    let bytes = parquet(&batch(0, Mode::Rows));
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
async fn manifest_api_remains_authoritative_when_statistics_omit_the_location() {
    let (connection, state) = service(Mode::NoManifest, None).await;
    let result = connection
        .query(SQL, &QueryOptions::default(), &QueryCancellation::new())
        .await
        .unwrap();
    assert_eq!(result.row_count(), 4);
    assert_eq!(result.schema().fields().len(), 7);
    assert_eq!(
        state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|op| *op == "GetQueryResults")
            .count(),
        2
    );
}
