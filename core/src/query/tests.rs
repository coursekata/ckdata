use super::*;
use aws_config::BehaviorVersion;
use aws_config::retry::RetryConfig;
use aws_credential_types::Credentials;
use aws_credential_types::provider::SharedCredentialsProvider;
use aws_sdk_athena::types::{
    EncryptionConfiguration, QueryExecutionStatistics, WorkGroupConfiguration,
};
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
fn query_text_preserves_sql_and_only_removes_a_terminal_delimiter() {
    for sql in [
        "SELECT '; DELETE FROM fake' AS \"odd; name\" ORDER BY 1",
        "WITH t AS (SELECT 1 AS x) SELECT x FROM t",
        "SELECT * FROM (SELECT 1 AS x ORDER BY x) t ORDER BY x DESC",
        "SELECT CAST(1 AS BIGINT) -- a trailing comment",
    ] {
        assert_eq!(query_text(sql).unwrap(), sql);
    }
    assert_eq!(query_text("  SELECT 1;  ").unwrap(), "SELECT 1");
    for sql in ["", "  ", ";"] {
        assert!(matches!(query_text(sql), Err(Error::InvalidQuery(_))));
    }
}

fn destination_configuration() -> ResultConfiguration {
    ResultConfiguration::builder()
        .output_location(OUTPUT)
        .encryption_configuration(
            EncryptionConfiguration::builder()
                .encryption_option(EncryptionOption::SseS3)
                .build()
                .unwrap(),
        )
        .expected_bucket_owner("000000000000")
        .build()
}

fn destination_workgroup(configuration: Option<ResultConfiguration>) -> WorkGroup {
    WorkGroup::builder()
        .name("synthetic")
        .state(WorkGroupState::Enabled)
        .configuration(
            WorkGroupConfiguration::builder()
                .set_result_configuration(configuration)
                .build(),
        )
        .build()
        .unwrap()
}

#[test]
fn destination_requires_an_enabled_workgroup_and_valid_output_settings() {
    assert!(matches!(
        QueryDestination::new(None, "token"),
        Err(Error::InvalidServiceResponse("GetWorkGroup"))
    ));
    let mut workgroup = destination_workgroup(Some(destination_configuration()));
    for state in [None, Some(WorkGroupState::Disabled)] {
        workgroup.state = state;
        assert!(matches!(
            QueryDestination::new(Some(&workgroup), "token"),
            Err(Error::InvalidSetting("workgroup must be enabled"))
        ));
    }
    let mut workgroup = destination_workgroup(None);
    for configuration in [workgroup.configuration.take(), None] {
        workgroup.configuration = configuration;
        assert!(matches!(
            QueryDestination::new(Some(&workgroup), "token"),
            Err(Error::InvalidSetting("workgroup result configuration"))
        ));
    }
    for output in [
        None,
        Some(""),
        Some("s3://synthetic-results/path/../other/"),
    ] {
        let configuration = ResultConfiguration::builder()
            .set_output_location(output.map(str::to_owned))
            .build();
        let workgroup = destination_workgroup(Some(configuration));
        assert!(matches!(
            QueryDestination::new(Some(&workgroup), "token"),
            Err(Error::InvalidSetting("workgroup output location"))
        ));
    }
    for (output, prefix) in [
        (
            "s3://synthetic-results/path",
            "s3://synthetic-results/path/",
        ),
        (
            "s3://synthetic-results/path//",
            "s3://synthetic-results/path//",
        ),
    ] {
        let configuration = ResultConfiguration::builder()
            .output_location(output)
            .build();
        let workgroup = destination_workgroup(Some(configuration.clone()));
        let destination = QueryDestination::new(Some(&workgroup), "token").unwrap();
        assert_eq!(destination.data_prefix, format!("{prefix}ckdata/token/"));
        assert_eq!(destination.configuration.output_location(), Some(output));
    }
}

#[test]
fn destination_accepts_only_verifiable_server_side_encryption() {
    for (option, key, expected_error) in [
        (EncryptionOption::SseS3, None, None),
        (
            EncryptionOption::SseKms,
            Some("12345678-1234-1234-1234-123456789abc"),
            None,
        ),
        (
            EncryptionOption::CseKms,
            None,
            Some("workgroup encryption must use server-side encryption"),
        ),
        (
            EncryptionOption::SseKms,
            None,
            Some("workgroup KMS key must be a key ARN or UUID"),
        ),
        (
            EncryptionOption::SseKms,
            Some("alias/synthetic"),
            Some("workgroup KMS key must be a key ARN or UUID"),
        ),
    ] {
        let mut configuration = destination_configuration();
        configuration.encryption_configuration = Some(
            EncryptionConfiguration::builder()
                .encryption_option(option)
                .set_kms_key(key.map(str::to_owned))
                .build()
                .unwrap(),
        );
        let workgroup = destination_workgroup(Some(configuration));
        match expected_error {
            Some(expected) => assert!(matches!(
                QueryDestination::new(Some(&workgroup), "token"),
                Err(Error::InvalidSetting(actual)) if actual == expected
            )),
            None => assert!(QueryDestination::new(Some(&workgroup), "token").is_ok()),
        }
    }
}

#[test]
fn destination_allows_an_absent_or_empty_manifest_but_requires_actual_result_output() {
    let configuration = destination_configuration();
    let workgroup = destination_workgroup(Some(configuration.clone()));
    let destination = QueryDestination::new(Some(&workgroup), "token").unwrap();
    for statistics in [
        None,
        Some(QueryExecutionStatistics::builder().build()),
        Some(
            QueryExecutionStatistics::builder()
                .data_manifest_location("")
                .build(),
        ),
    ] {
        let execution = AthenaQueryExecution::builder()
            .result_configuration(configuration.clone())
            .set_statistics(statistics)
            .build();
        let completed = destination.complete(QUERY_ID, &execution).unwrap();
        assert_eq!(completed.query_id, QUERY_ID);
        assert_eq!(completed.manifest_location, None);
        assert_eq!(completed.data_prefix, format!("{OUTPUT}ckdata/token/"));
    }
    let mut missing_output = configuration.clone();
    missing_output.output_location = None;
    let mut empty_output = configuration;
    empty_output.output_location = Some(String::new());
    for actual in [None, Some(missing_output), Some(empty_output)] {
        let execution = AthenaQueryExecution::builder()
            .set_result_configuration(actual)
            .build();
        assert!(matches!(
            destination.complete(QUERY_ID, &execution),
            Err(QueryInterruption::InvalidResponse(
                "query result configuration"
            ))
        ));
    }
}

#[test]
fn destination_uses_the_same_output_boundary_for_manifest_and_result_locations() {
    let configuration = destination_configuration();
    let workgroup = destination_workgroup(Some(configuration.clone()));
    let destination = QueryDestination::new(Some(&workgroup), "token").unwrap();
    for (location, valid) in [
        (format!("{OUTPUT}query.txt"), true),
        (format!("{OUTPUT}nested/query.txt"), true),
        (
            "s3://synthetic-results/results/readers/query.txt".into(),
            false,
        ),
        (
            "s3://other-synthetic/results/reader/query.txt".into(),
            false,
        ),
        (format!("{OUTPUT}../query.txt"), false),
        (format!("{OUTPUT}%2e%2e/query.txt"), false),
        (format!("{OUTPUT}query.txt?query"), false),
        (format!("{OUTPUT}query.txt\n"), false),
        ("https://synthetic/results/reader/query.txt".into(), false),
    ] {
        let execution = AthenaQueryExecution::builder()
            .result_configuration(configuration.clone())
            .statistics(
                QueryExecutionStatistics::builder()
                    .data_manifest_location(&location)
                    .build(),
            )
            .build();
        let result = destination.complete(QUERY_ID, &execution);
        assert_eq!(result.is_ok(), valid, "manifest {location:?}");
        if !valid {
            assert!(matches!(
                result,
                Err(QueryInterruption::InvalidResponse("manifest location"))
            ));
        }
        let mut actual = configuration.clone();
        actual.output_location = Some(location.clone());
        let execution = AthenaQueryExecution::builder()
            .result_configuration(actual)
            .build();
        let result = destination.complete(QUERY_ID, &execution);
        assert_eq!(result.is_ok(), valid, "result {location:?}");
        if !valid {
            assert!(matches!(
                result,
                Err(QueryInterruption::InvalidResponse(
                    "query result configuration"
                ))
            ));
        }
    }
}

#[test]
fn destination_requires_matching_encryption_and_bucket_owner() {
    let configuration = destination_configuration();
    let workgroup = destination_workgroup(Some(configuration.clone()));
    let destination = QueryDestination::new(Some(&workgroup), "token").unwrap();
    let mut no_encryption = configuration.clone();
    no_encryption.encryption_configuration = None;
    let mut different_encryption = configuration.clone();
    different_encryption.encryption_configuration = Some(
        EncryptionConfiguration::builder()
            .encryption_option(EncryptionOption::SseKms)
            .kms_key("12345678-1234-1234-1234-123456789abc")
            .build()
            .unwrap(),
    );
    let mut no_owner = configuration.clone();
    no_owner.expected_bucket_owner = None;
    let mut different_owner = configuration;
    different_owner.expected_bucket_owner = Some("111111111111".into());
    for actual in [
        no_encryption,
        different_encryption,
        no_owner,
        different_owner,
    ] {
        let execution = AthenaQueryExecution::builder()
            .result_configuration(actual)
            .build();
        assert!(matches!(
            destination.complete(QUERY_ID, &execution),
            Err(QueryInterruption::InvalidResponse(
                "query result configuration"
            ))
        ));
    }
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
            .execute_unload("  ; ", &QueryOptions::default(), &token)
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
            "SELECT 1 AS x ORDER BY x DESC -- trailing",
            &QueryOptions::default(),
            &QueryCancellation::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.query_id, QUERY_ID);
    assert_eq!(
        result.manifest_location,
        Some(format!("{OUTPUT}{QUERY_ID}-manifest.csv"))
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
            "UNLOAD (\nSELECT 1 AS x ORDER BY x DESC -- trailing\n) TO '{}' WITH (format = 'PARQUET', compression = 'SNAPPY')",
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
async fn submission_errors_retain_token_and_modeled_guidance_without_signature_details() {
    for response in [
        json!({"__type": "InvalidRequestException", "Message": "check the configured output location"}),
        json!({"__type": "SignatureDoesNotMatch", "Message": "synthetic signed authentication detail"}),
        json!({}),
    ] {
        let status = if response.get("__type").is_some() {
            403
        } else {
            200
        };
        let expected_code = response["__type"].as_str().map(str::to_owned);
        let expected_message = if expected_code.as_deref() == Some("InvalidRequestException") {
            Some("check the configured output location")
        } else {
            None
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
        assert!(!format!("{error:?} {error}").contains("synthetic signed authentication detail"));
        let Error::QuerySubmission {
            code,
            message,
            request_token,
            outcome_unknown,
        } = &error
        else {
            panic!("expected a submission failure, got {error:?}");
        };
        assert_eq!(message.as_deref(), expected_message);
        assert_eq!(*outcome_unknown, expected_code.is_none());
        assert_eq!(error.remote_query_may_be_running(), *outcome_unknown);
        if let Some(expected_code) = expected_code {
            assert_eq!(code, &expected_code);
        } else {
            assert_eq!(code, "missing query ID; submission outcome unknown");
        }
        assert_eq!(requests(&http)[1].1["ClientRequestToken"], *request_token);
    }
}

#[tokio::test]
async fn submission_transport_failure_retains_token_and_marks_the_outcome_unknown() {
    use aws_smithy_http_client::test_util::dvr::RecordingClient;

    // Record the attempted submission even though it exhausts the replay and
    // receives a connector error instead of a service response.
    let http = RecordingClient::new(replay(vec![(200, workgroup())]));
    let connection = Connection::from_config(
        ConnectionOptions::new("synthetic", "synthetic_schema"),
        config().http_client(http.clone()).build(),
    )
    .await
    .unwrap();
    let error = connection
        .execute_unload(
            "SELECT 1",
            &QueryOptions::default(),
            &QueryCancellation::new(),
        )
        .await
        .unwrap_err();
    let Error::QuerySubmission {
        code,
        message,
        request_token,
        outcome_unknown,
    } = &error
    else {
        panic!("expected a submission failure, got {error:?}");
    };
    assert_eq!(code, "request failed; submission outcome unknown");
    assert_eq!(message, &None);
    assert!(Uuid::parse_str(request_token).is_ok());
    assert!(*outcome_unknown);
    assert!(error.remote_query_may_be_running());
    // The recorder copies request bodies in a background task. Wait for the
    // failed submission's complete body before checking its idempotency token.
    let events = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let traffic = serde_json::to_value(http.network_traffic()).unwrap();
            let events = traffic["events"].as_array().unwrap();
            if events.iter().any(|event| {
                event["connection_id"].as_u64() == Some(1)
                    && event["action"]["Eof"]["direction"] == "Request"
            }) {
                break events.clone();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the recorder must capture the failed submission body");
    assert_eq!(
        events
            .iter()
            .filter_map(|event| event["action"].get("Request"))
            .map(|request| {
                request["request"]["headers"]["x-amz-target"][0]
                    .as_str()
                    .unwrap()
            })
            .collect::<Vec<_>>(),
        [
            "AmazonAthena.GetWorkGroup",
            "AmazonAthena.StartQueryExecution"
        ]
    );
    let body = events
        .iter()
        .filter(|event| {
            event["connection_id"].as_u64() == Some(1)
                && event["action"]["Data"]["direction"] == "Request"
        })
        .map(|event| event["action"]["Data"]["data"]["Utf8"].as_str().unwrap())
        .collect::<String>();
    let submitted: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(submitted["ClientRequestToken"], *request_token);
    assert!(events.iter().any(|event| {
        event["connection_id"].as_u64() == Some(1)
            && event["action"]["Response"]["response"]["Err"].is_string()
    }));
}

#[tokio::test]
async fn polling_failure_unknown_state_and_misdirected_manifest_request_a_stop() {
    let mut bad_manifest = execution("SUCCEEDED");
    bad_manifest["QueryExecution"]["Statistics"]["DataManifestLocation"] =
        json!("s3://other-synthetic/elsewhere/manifest.csv");
    for (status, response) in [
        (
            403,
            json!({"__type": "AccessDeniedException", "Message": "check the workgroup permission"}),
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
        if status == 403 {
            assert!(
                matches!(&error, Error::QueryInterrupted { reason: QueryInterruption::RequestFailed { code, message, .. }, .. } if code == "AccessDeniedException" && message.as_deref() == Some("check the workgroup permission"))
            );
            assert!(error.to_string().contains("check the workgroup permission"));
        }
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
