use super::*;
use aws_config::BehaviorVersion;
use aws_config::retry::RetryConfig;
use aws_credential_types::Credentials;
use aws_credential_types::provider::SharedCredentialsProvider;
use aws_smithy_async::time::StaticTimeSource;
use aws_smithy_http_client::test_util::{
    NeverClient, ReplayEvent, StaticReplayClient, infallible_client_fn,
};
use aws_smithy_types::body::SdkBody;
use aws_types::{SdkConfig, region::Region};
use serde_json::{Value, json};

use crate::ConnectionOptions;

const OUTPUT: &str = "s3://synthetic-results/results/reader/";
const QUERY_ID: &str = "synthetic-query";

fn configuration() -> Value {
    json!({"OutputLocation": OUTPUT, "EncryptionConfiguration": {"EncryptionOption": "SSE_S3"}, "ExpectedBucketOwner": "000000000000"})
}

fn workgroup() -> Value {
    json!({"WorkGroup": {"Name": "synthetic", "State": "ENABLED", "Configuration": {"ResultConfiguration": configuration()}}})
}

fn execution(state: &str) -> Value {
    let mut config = configuration();
    config["OutputLocation"] = json!(format!("{OUTPUT}{QUERY_ID}.txt"));
    json!({"QueryExecution": {"QueryExecutionId": QUERY_ID, "WorkGroup": "synthetic", "Status": {"State": state, "StateChangeReason": "synthetic failure reason"}, "Statistics": {"DataManifestLocation": format!("{OUTPUT}{QUERY_ID}-manifest.csv")}, "ResultConfiguration": config}})
}

fn config() -> aws_types::sdk_config::Builder {
    SdkConfig::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new("us-west-2"))
        .credentials_provider(SharedCredentialsProvider::new(Credentials::new(
            "SYNTHETIC",
            "synthetic-secret",
            None,
            None,
            "synthetic-test",
        )))
        .time_source(StaticTimeSource::from_secs(1_704_067_200))
        .retry_config(RetryConfig::standard().with_max_attempts(1))
}

fn replay(responses: Vec<(u16, Value)>) -> StaticReplayClient {
    StaticReplayClient::new(
        responses
            .into_iter()
            .map(|(status, body)| {
                ReplayEvent::new(
                    http::Request::new(SdkBody::empty()),
                    http::Response::builder()
                        .status(status)
                        .header("content-type", "application/x-amz-json-1.1")
                        .body(SdkBody::from(body.to_string()))
                        .unwrap(),
                )
            })
            .collect(),
    )
}

async fn connection(http: StaticReplayClient) -> Connection {
    Connection::from_config(
        ConnectionOptions::new("synthetic", "synthetic_schema"),
        config().http_client(http).build(),
    )
    .await
    .unwrap()
}

fn requests(http: &StaticReplayClient) -> Vec<(String, Value)> {
    http.actual_requests()
        .map(|request| {
            (
                request
                    .headers()
                    .get("x-amz-target")
                    .unwrap()
                    .rsplit('.')
                    .next()
                    .unwrap()
                    .to_owned(),
                serde_json::from_slice(request.body().bytes().unwrap()).unwrap(),
            )
        })
        .collect()
}

#[test]
fn parses_one_read_only_query_without_losing_nested_ordering_or_quoted_text() {
    for sql in [
        "SELECT NULL AS s_null, '' AS s_empty, 9007199254740993 AS big",
        "WITH t AS (SELECT 1 AS x) SELECT x FROM t ORDER BY x DESC NULLS FIRST; -- end",
        "SELECT '; DELETE FROM fake' AS \"odd; name\" ORDER BY 1",
        "SELECT * FROM (SELECT 1 AS x ORDER BY x) t ORDER BY x DESC",
        "SELECT 1 UNION ALL SELECT 2 ORDER BY 1",
        "VALUES (1), (2)",
    ] {
        let query = read_query(sql).unwrap();
        assert!(read_query(&query.to_string()).is_ok());
    }
    let query = read_query("SELECT * FROM (SELECT 1 AS x ORDER BY x) t ORDER BY x DESC").unwrap();
    assert_eq!(query.order_by.unwrap().to_string(), "ORDER BY x DESC");
}

#[test]
fn rejects_multiple_statements_nested_writes_and_locks_without_echoing_sql() {
    for sql in [
        "",
        ";",
        "SELECT 1; SELECT 2",
        "DELETE FROM synthetic",
        "CREATE TABLE synthetic AS SELECT 1",
        "EXPLAIN SELECT 1",
        "SELECT 1 INTO synthetic",
        "SELECT * FROM (SELECT 1 INTO synthetic) t",
        "WITH x AS (DELETE FROM synthetic RETURNING *) SELECT * FROM x",
        "SELECT * FROM synthetic FOR UPDATE",
        "SELECT * FROM (SELECT * FROM synthetic FOR UPDATE) t",
        "SELECT * FROM synthetic SETTINGS x = 1",
        "SELECT 'private-synthetic-marker' +",
    ] {
        let error = read_query(sql).unwrap_err();
        assert!(matches!(error, Error::InvalidQuery(_)), "{sql}");
        assert!(!error.to_string().contains("private-synthetic-marker"));
    }
    assert!(read_query(&format!("SELECT {}1{}", "(".repeat(100), ")".repeat(100))).is_err());
}

#[tokio::test]
async fn invalid_sql_options_and_pre_cancelled_queries_make_no_requests() {
    let connection = Connection::from_config(
        ConnectionOptions::new("synthetic", "schema"),
        config().http_client(NeverClient::new()).build(),
    )
    .await
    .unwrap();
    let token = QueryCancellation::new();
    assert!(matches!(
        connection
            .execute_unload("DROP TABLE synthetic", &QueryOptions::default(), &token)
            .await,
        Err(Error::InvalidQuery(_))
    ));
    let invalid = QueryOptions {
        timeout: Duration::ZERO,
        ..QueryOptions::default()
    };
    assert!(matches!(
        connection
            .execute_unload("SELECT 1", &invalid, &token)
            .await,
        Err(Error::InvalidSetting(_))
    ));
    token.cancel();
    assert!(matches!(
        connection
            .execute_unload("SELECT 1", &QueryOptions::default(), &token)
            .await,
        Err(Error::QueryInterrupted {
            query_id: None,
            reason: QueryInterruption::Cancelled,
            cancellation: CancellationStatus::NotSubmitted
        })
    ));
}

#[tokio::test(start_paused = true)]
async fn unload_uses_workgroup_settings_unique_output_and_tracks_state_regressions() {
    let http = replay(vec![
        (200, workgroup()),
        (200, json!({"QueryExecutionId": QUERY_ID})),
        (200, execution("RUNNING")),
        (200, execution("QUEUED")),
        (200, execution("SUCCEEDED")),
    ]);
    let result = connection(http.clone())
        .await
        .execute_unload(
            "SELECT 1 AS x ORDER BY x DESC; -- trailing",
            &QueryOptions::default(),
            &QueryCancellation::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.query_id, QUERY_ID);
    assert_eq!(
        result.manifest_location,
        format!("{OUTPUT}{QUERY_ID}-manifest.csv")
    );
    let requests = requests(&http);
    assert_eq!(
        requests
            .iter()
            .map(|(target, _)| target.as_str())
            .collect::<Vec<_>>(),
        [
            "GetWorkGroup",
            "StartQueryExecution",
            "GetQueryExecution",
            "GetQueryExecution",
            "GetQueryExecution"
        ]
    );
    let start = &requests[1].1;
    assert_eq!(start["WorkGroup"], "synthetic");
    assert_eq!(
        start["QueryExecutionContext"],
        json!({"Catalog": "AwsDataCatalog", "Database": "synthetic_schema"})
    );
    assert_eq!(start["ResultConfiguration"], configuration());
    let token = start["ClientRequestToken"].as_str().unwrap();
    assert!(Uuid::parse_str(token).is_ok());
    assert_eq!(result.data_prefix, format!("{OUTPUT}ckdata/{token}/"));
    assert_eq!(
        start["QueryString"],
        format!(
            "UNLOAD (SELECT 1 AS x ORDER BY x DESC) TO '{}' WITH (format = 'PARQUET', compression = 'SNAPPY')",
            result.data_prefix
        )
    );
}

#[tokio::test]
async fn athena_failure_retains_reason_and_query_id_without_an_unnecessary_stop() {
    let http = replay(vec![
        (200, workgroup()),
        (200, json!({"QueryExecutionId": QUERY_ID})),
        (200, execution("FAILED")),
    ]);
    let error = connection(http.clone())
        .await
        .execute_unload(
            "SELECT 1",
            &QueryOptions::default(),
            &QueryCancellation::new(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::QueryFailed { query_id, reason } if query_id == QUERY_ID && reason == "synthetic failure reason")
    );
    assert_eq!(requests(&http).len(), 3);
}

#[tokio::test]
async fn remote_cancelled_is_terminal_and_does_not_issue_another_stop() {
    let http = replay(vec![
        (200, workgroup()),
        (200, json!({"QueryExecutionId": QUERY_ID})),
        (200, execution("CANCELLED")),
    ]);
    let error = connection(http.clone())
        .await
        .execute_unload(
            "SELECT 1",
            &QueryOptions::default(),
            &QueryCancellation::new(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::QueryInterrupted { query_id: Some(id), reason: QueryInterruption::Cancelled, cancellation: CancellationStatus::Requested } if id == QUERY_ID)
    );
    assert_eq!(requests(&http).len(), 3);
}

#[tokio::test(start_paused = true)]
async fn deadline_stops_the_submitted_query_and_preserves_timeout_if_stop_fails() {
    for stop_status in [200, 403] {
        let http = replay(vec![
            (200, workgroup()),
            (200, json!({"QueryExecutionId": QUERY_ID})),
            (200, execution("RUNNING")),
            (
                stop_status,
                json!({"__type": "AccessDeniedException", "Message": "synthetic response detail"}),
            ),
        ]);
        let options = QueryOptions {
            timeout: Duration::from_secs(2),
            poll_interval: Duration::from_secs(10),
            ..QueryOptions::default()
        };
        let error = connection(http.clone())
            .await
            .execute_unload("SELECT 1", &options, &QueryCancellation::new())
            .await
            .unwrap_err();
        let expected = if stop_status == 200 {
            CancellationStatus::Requested
        } else {
            CancellationStatus::Unconfirmed
        };
        assert!(
            matches!(error, Error::QueryInterrupted { query_id: Some(id), reason: QueryInterruption::TimedOut, cancellation } if id == QUERY_ID && cancellation == expected)
        );
        assert_eq!(
            requests(&http).last().unwrap(),
            &(
                "StopQueryExecution".into(),
                json!({"QueryExecutionId": QUERY_ID})
            )
        );
    }
}

#[tokio::test]
async fn cancellation_during_submission_waits_for_query_id_then_stops() {
    let token = QueryCancellation::new();
    let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let calls = observed.clone();
    let cancel = token.clone();
    let http = infallible_client_fn(move |request| {
        let target = request.headers()["x-amz-target"]
            .to_str()
            .unwrap()
            .rsplit('.')
            .next()
            .unwrap()
            .to_owned();
        calls.lock().unwrap().push(target.clone());
        let body = match target.as_str() {
            "GetWorkGroup" => workgroup(),
            "StartQueryExecution" => {
                cancel.cancel();
                json!({"QueryExecutionId": QUERY_ID})
            }
            "StopQueryExecution" => json!({}),
            other => panic!("unexpected request {other}"),
        };
        http::Response::builder()
            .status(200)
            .header("content-type", "application/x-amz-json-1.1")
            .body(body.to_string())
            .unwrap()
    });
    let connection = Connection::from_config(
        ConnectionOptions::new("synthetic", "schema"),
        config().http_client(http).build(),
    )
    .await
    .unwrap();
    let error = connection
        .execute_unload("SELECT 1", &QueryOptions::default(), &token)
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::QueryInterrupted { query_id: Some(id), reason: QueryInterruption::Cancelled, cancellation: CancellationStatus::Requested } if id == QUERY_ID)
    );
    assert_eq!(
        *observed.lock().unwrap(),
        ["GetWorkGroup", "StartQueryExecution", "StopQueryExecution"]
    );
}

#[tokio::test]
async fn submission_errors_retain_idempotency_token_without_raw_service_details() {
    for response in [
        json!({"__type": "AccessDeniedException", "Message": "sensitive-synthetic-details"}),
        json!({}),
    ] {
        let status = if response.get("__type").is_some() {
            403
        } else {
            200
        };
        let http = replay(vec![(200, workgroup()), (status, response)]);
        let error = connection(http.clone())
            .await
            .execute_unload(
                "SELECT 1",
                &QueryOptions::default(),
                &QueryCancellation::new(),
            )
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("sensitive-synthetic-details"));
        assert!(
            matches!(error, Error::QuerySubmission { request_token, .. } if requests(&http)[1].1["ClientRequestToken"] == request_token)
        );
    }
}

#[tokio::test]
async fn polling_failure_unknown_state_and_misdirected_manifest_request_a_stop() {
    let mut bad_manifest = execution("SUCCEEDED");
    bad_manifest["QueryExecution"]["Statistics"]["DataManifestLocation"] =
        json!("s3://other-synthetic/elsewhere/manifest.csv");
    for (status, response) in [
        (
            403,
            json!({"__type": "AccessDeniedException", "Message": "sensitive-synthetic-details"}),
        ),
        (200, execution("UNKNOWN_FUTURE_STATE")),
        (200, bad_manifest),
    ] {
        let http = replay(vec![
            (200, workgroup()),
            (200, json!({"QueryExecutionId": QUERY_ID})),
            (status, response),
            (200, json!({})),
        ]);
        let error = connection(http.clone())
            .await
            .execute_unload(
                "SELECT 1",
                &QueryOptions::default(),
                &QueryCancellation::new(),
            )
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("sensitive-synthetic-details"));
        assert!(
            matches!(error, Error::QueryInterrupted { query_id: Some(id), cancellation: CancellationStatus::Requested, .. } if id == QUERY_ID)
        );
        assert_eq!(requests(&http).last().unwrap().0, "StopQueryExecution");
    }
}

#[tokio::test]
async fn invalid_workgroup_settings_fail_before_submission() {
    let mut disabled = workgroup();
    disabled["WorkGroup"]["State"] = json!("DISABLED");
    let mut no_output = workgroup();
    no_output["WorkGroup"]["Configuration"]["ResultConfiguration"]
        .as_object_mut()
        .unwrap()
        .remove("OutputLocation");
    let mut client_encryption = workgroup();
    client_encryption["WorkGroup"]["Configuration"]["ResultConfiguration"]["EncryptionConfiguration"]
        ["EncryptionOption"] = json!("CSE_KMS");
    for body in [disabled, no_output, client_encryption] {
        let http = replay(vec![(200, body)]);
        assert!(matches!(
            connection(http.clone())
                .await
                .execute_unload(
                    "SELECT 1",
                    &QueryOptions::default(),
                    &QueryCancellation::new()
                )
                .await,
            Err(Error::InvalidSetting(_))
        ));
        assert_eq!(requests(&http).len(), 1);
    }
    assert_eq!(
        s3_prefix("s3://synthetic-results/path//"),
        Some("s3://synthetic-results/path//".into())
    );
    for invalid in [
        "https://synthetic/results/",
        "s3://synthetic/path/../other/",
        "s3://synthetic/path?query",
        "s3://synthetic/path\n",
    ] {
        assert!(!s3_location(invalid));
    }
}

#[tokio::test(start_paused = true)]
async fn guard_interrupts_an_inflight_request_without_waiting_for_its_response() {
    let connection = Connection::from_config(
        ConnectionOptions::new("synthetic", "schema"),
        config().http_client(NeverClient::new()).build(),
    )
    .await
    .unwrap();
    let token = QueryCancellation::new();
    let guard = RequestGuard {
        connection: &connection,
        cancellation: &token,
        deadline: Instant::now() + Duration::from_secs(2),
    };
    let result: Result<(), _> = guard.run(std::future::pending()).await;
    assert_eq!(result, Err(QueryInterruption::TimedOut));
    let guard = RequestGuard {
        connection: &connection,
        cancellation: &token,
        deadline: Instant::now() + Duration::from_secs(30),
    };
    let (result, ()) = tokio::join!(guard.run::<()>(std::future::pending()), async {
        sleep(Duration::from_secs(1)).await;
        token.cancel();
    });
    assert_eq!(result, Err(QueryInterruption::Cancelled));
}

#[tokio::test(start_paused = true)]
async fn sdk_transient_retries_keep_submission_token_and_destination_unchanged() {
    let http = replay(vec![
        (200, workgroup()),
        (500, json!({"__type": "InternalServerException"})),
        (200, json!({"QueryExecutionId": QUERY_ID})),
        (500, json!({"__type": "InternalServerException"})),
        (200, execution("SUCCEEDED")),
    ]);
    let connection = Connection::from_config(
        ConnectionOptions::new("synthetic", "schema"),
        config()
            .retry_config(RetryConfig::standard().with_max_attempts(2))
            .http_client(http.clone())
            .build(),
    )
    .await
    .unwrap();
    connection
        .execute_unload(
            "SELECT 1",
            &QueryOptions::default(),
            &QueryCancellation::new(),
        )
        .await
        .unwrap();
    let requests = requests(&http);
    assert_eq!(requests.len(), 5);
    assert_eq!(requests[1].0, "StartQueryExecution");
    assert_eq!(requests[1].1, requests[2].1);
    assert_eq!(requests[3].0, "GetQueryExecution");
    assert_eq!(requests[3].1, requests[4].1);
}

#[tokio::test(start_paused = true)]
async fn role_expiry_interrupts_polling_and_does_not_fall_back_to_source_credentials() {
    let mut events = vec![ReplayEvent::new(http::Request::new(SdkBody::empty()), http::Response::builder()
        .status(200).header("content-type", "text/xml").body(SdkBody::from(r#"<AssumeRoleResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/"><AssumeRoleResult><Credentials><AccessKeyId>ASSUMED_SYNTHETIC</AccessKeyId><SecretAccessKey>synthetic-role-secret</SecretAccessKey><SessionToken>synthetic-role-token</SessionToken><Expiration>2024-01-01T00:00:02Z</Expiration></Credentials></AssumeRoleResult></AssumeRoleResponse>"#)).unwrap())];
    for body in [
        workgroup(),
        json!({"QueryExecutionId": QUERY_ID}),
        execution("RUNNING"),
    ] {
        events.push(ReplayEvent::new(
            http::Request::new(SdkBody::empty()),
            http::Response::builder()
                .status(200)
                .header("content-type", "application/x-amz-json-1.1")
                .body(SdkBody::from(body.to_string()))
                .unwrap(),
        ));
    }
    let http = StaticReplayClient::new(events);
    let mut options = ConnectionOptions::new("synthetic", "schema");
    options.role_arn = Some("arn:aws:iam::000000000000:role/synthetic-reader".into());
    let connection = Connection::from_config(options, config().http_client(http.clone()).build())
        .await
        .unwrap();
    let options = QueryOptions {
        poll_interval: Duration::from_secs(10),
        ..QueryOptions::default()
    };
    let error = connection
        .execute_unload("SELECT 1", &options, &QueryCancellation::new())
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::QueryInterrupted { query_id: Some(id), reason: QueryInterruption::SessionExpired, cancellation: CancellationStatus::Unconfirmed } if id == QUERY_ID)
    );
    assert_eq!(http.actual_requests().count(), 4);
    for request in http.actual_requests().skip(1) {
        assert!(
            request
                .headers()
                .get("authorization")
                .unwrap()
                .contains("ASSUMED_SYNTHETIC")
        );
    }
    assert!(matches!(
        connection.session_status(),
        Err(Error::ExpiredRole)
    ));
}
