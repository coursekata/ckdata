use super::*;
use aws_config::retry::RetryConfig;
use aws_smithy_async::time::StaticTimeSource;
use aws_smithy_http_client::test_util::{NeverClient, ReplayEvent, StaticReplayClient};
use aws_smithy_types::body::SdkBody;
use aws_types::os_shim_internal::{Env, Fs};

const EPOCH: u64 = 1_704_067_200;
const ROLE: &str = "arn:aws:iam::000000000000:role/synthetic-reader";

fn options() -> ConnectionOptions {
    ConnectionOptions::new("synthetic-workgroup", "synthetic_schema")
}

fn loader(extra: &[(&str, &str)]) -> ConfigLoader {
    let mut env = vec![
        ("AWS_CONFIG_FILE", "/synthetic/config"),
        ("AWS_SHARED_CREDENTIALS_FILE", "/synthetic/credentials"),
        ("AWS_EC2_METADATA_DISABLED", "true"),
    ];
    env.extend_from_slice(extra);
    aws_config::defaults(BehaviorVersion::latest())
        .env(Env::from_slice(&env))
        .fs(Fs::from_slice(&[
            ("/synthetic/config", "[profile analyst]\nregion = eu-west-1\n[default]\nregion = us-west-2\n"),
            ("/synthetic/credentials", "[analyst]\naws_access_key_id = PROFILE_KEY\naws_secret_access_key = synthetic-profile-secret\n"),
        ]))
        .http_client(NeverClient::new())
}

#[tokio::test]
async fn standard_sources_keep_environment_credentials_ahead_of_named_profile() {
    let mut opts = options();
    opts.profile = Some("analyst".into());
    let config = load_config(
        &opts,
        loader(&[
            ("AWS_ACCESS_KEY_ID", "ENVIRONMENT_KEY"),
            ("AWS_SECRET_ACCESS_KEY", "synthetic-environment-secret"),
            ("AWS_REGION", "ap-southeast-2"),
        ]),
    )
    .await
    .unwrap();
    let credentials = config
        .credentials_provider()
        .unwrap()
        .provide_credentials()
        .await
        .unwrap();
    assert_eq!(credentials.access_key_id(), "ENVIRONMENT_KEY");
    assert_eq!(config.region().unwrap().as_ref(), "ap-southeast-2");
    let connection = Connection::from_config(opts, config).await.unwrap();
    assert_eq!(
        connection.session_status().unwrap(),
        SessionStatus::Standard
    );
}

#[tokio::test]
async fn explicit_profile_and_region_override_discovery_without_changing_credentials() {
    let mut opts = options();
    opts.profile = Some("analyst".into());
    opts.region = Some("eu-central-1".into());
    let config = load_config(&opts, loader(&[("AWS_REGION", "ap-southeast-2")]))
        .await
        .unwrap();
    let credentials = config
        .credentials_provider()
        .unwrap()
        .provide_credentials()
        .await
        .unwrap();
    assert_eq!(credentials.access_key_id(), "PROFILE_KEY");
    let connection = Connection::from_config(opts, config).await.unwrap();
    assert_eq!(connection.region(), "eu-central-1");
}

#[tokio::test]
async fn profile_environment_selects_the_profile_without_an_explicit_override() {
    let connection = Connection::with_loader(options(), loader(&[("AWS_PROFILE", "analyst")]))
        .await
        .unwrap();
    assert_eq!(connection.region(), "eu-west-1");
    let credentials = connection
        .config
        .credentials_provider()
        .unwrap()
        .provide_credentials()
        .await
        .unwrap();
    assert_eq!(credentials.access_key_id(), "PROFILE_KEY");
}

#[tokio::test]
async fn missing_credentials_and_invalid_settings_fail_without_network_or_real_files() {
    let error = Connection::with_loader(options(), loader(&[]))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::CredentialsUnavailable));
    for opts in [
        ConnectionOptions::new("", "schema"),
        ConnectionOptions::new("workgroup", ""),
        ConnectionOptions {
            role_arn: Some("arn:aws:iam::000000000000:user/not-a-role".into()),
            ..options()
        },
    ] {
        assert!(matches!(
            Connection::with_loader(opts, loader(&[])).await,
            Err(Error::InvalidSetting(_))
        ));
    }
}

#[tokio::test(start_paused = true)]
async fn standard_connections_retain_their_refreshing_source_provider() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Debug)]
    struct Rotating(Arc<AtomicUsize>);
    impl ProvideCredentials for Rotating {
        fn provide_credentials<'a>(&'a self) -> provider::future::ProvideCredentials<'a>
        where
            Self: 'a,
        {
            let generation = self.0.fetch_add(1, Ordering::SeqCst);
            provider::future::ProvideCredentials::ready(Ok(Credentials::new(
                format!("SOURCE_{generation}"),
                "synthetic-secret",
                None,
                None,
                "synthetic-refresh",
            )))
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let config = SdkConfig::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new("us-west-2"))
        .credentials_provider(SharedCredentialsProvider::new(Rotating(calls.clone())))
        .http_client(NeverClient::new())
        .build();
    let connection = Connection::from_config(options(), config).await.unwrap();
    tokio::time::advance(Duration::from_secs(7200)).await;
    let refreshed = connection
        .config
        .credentials_provider()
        .unwrap()
        .provide_credentials()
        .await
        .unwrap();
    assert_eq!(refreshed.access_key_id(), "SOURCE_1");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        connection.session_status().unwrap(),
        SessionStatus::Standard
    );
}

#[tokio::test]
async fn no_region_fails_before_any_service_request() {
    let config = SdkConfig::builder()
        .behavior_version(BehaviorVersion::latest())
        .credentials_provider(SharedCredentialsProvider::new(Credentials::new(
            "SYNTHETIC",
            "synthetic-secret",
            None,
            None,
            "test",
        )))
        .http_client(NeverClient::new())
        .build();
    assert!(matches!(
        Connection::from_config(options(), config).await,
        Err(Error::MissingRegion)
    ));
}

fn response(expiration: &str) -> String {
    format!(
        r#"<AssumeRoleResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/">
      <AssumeRoleResult><Credentials>
        <AccessKeyId>ASSUMED_KEY</AccessKeyId>
        <SecretAccessKey>synthetic-role-secret</SecretAccessKey>
        <SessionToken>synthetic-role-token</SessionToken>
        <Expiration>{expiration}</Expiration>
      </Credentials></AssumeRoleResult>
      <ResponseMetadata><RequestId>synthetic-request</RequestId></ResponseMetadata>
    </AssumeRoleResponse>"#
    )
}

fn config(http: StaticReplayClient) -> SdkConfig {
    SdkConfig::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new("us-west-2"))
        .credentials_provider(SharedCredentialsProvider::new(Credentials::new(
            "SOURCE_KEY",
            "synthetic-source-secret",
            None,
            None,
            "synthetic",
        )))
        .time_source(StaticTimeSource::from_secs(EPOCH))
        .retry_config(RetryConfig::standard().with_max_attempts(1))
        .http_client(http)
        .build()
}

fn replay(status: u16, body: String) -> StaticReplayClient {
    StaticReplayClient::new(vec![ReplayEvent::new(
        http::Request::new(SdkBody::empty()),
        http::Response::builder()
            .status(status)
            .header("content-type", "text/xml")
            .body(SdkBody::from(body))
            .unwrap(),
    )])
}

#[tokio::test(start_paused = true)]
async fn one_role_assumption_warns_at_fifteen_minutes_and_cannot_renew_at_expiry() {
    let http = replay(200, response("2024-01-01T01:00:00Z"));
    let opts = ConnectionOptions {
        role_arn: Some(ROLE.into()),
        ..options()
    };
    let connection = Connection::from_config(opts, config(http.clone()))
        .await
        .unwrap();
    let clone = connection.clone();
    assert!(matches!(
        connection.session_status(),
        Ok(SessionStatus::Assumed { .. })
    ));
    let provider = connection.config.credentials_provider().unwrap();
    assert_eq!(
        provider
            .provide_credentials()
            .await
            .unwrap()
            .access_key_id(),
        "ASSUMED_KEY"
    );

    tokio::time::advance(Duration::from_secs(2699)).await;
    assert!(matches!(
        connection.session_status(),
        Ok(SessionStatus::Assumed { .. })
    ));
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(
        matches!(connection.session_status(), Ok(SessionStatus::Expiring { remaining, .. }) if remaining == WARNING_WINDOW)
    );
    tokio::time::advance(WARNING_WINDOW).await;
    assert!(matches!(
        connection.session_status(),
        Err(Error::ExpiredRole)
    ));
    assert!(matches!(clone.session_status(), Err(Error::ExpiredRole)));
    assert!(provider.provide_credentials().await.is_err());
    assert!(provider.fallback_on_interrupt().is_none());
    // The SDK must reject the expired snapshot before reaching even a mock
    // connector. NeverClient panics if any HTTP request is attempted.
    let athena = aws_sdk_athena::Client::from_conf(
        aws_sdk_athena::config::Builder::from(&connection.config)
            .http_client(NeverClient::new())
            .build(),
    );
    assert!(
        athena
            .get_work_group()
            .work_group("synthetic")
            .send()
            .await
            .is_err()
    );
    assert_eq!(
        http.actual_requests().count(),
        1,
        "expiration must not call STS again"
    );
    let requests: Vec<_> = http
        .actual_requests()
        .map(|request| String::from_utf8_lossy(request.body().bytes().unwrap()).into_owned())
        .collect();
    assert!(requests[0].contains("DurationSeconds=3600"));
    assert!(
        requests[0].contains("RoleArn=arn%3Aaws%3Aiam%3A%3A000000000000%3Arole%2Fsynthetic-reader")
    );
    let debug = format!("{connection:?}");
    assert!(!debug.contains("synthetic-role-secret"));
    assert!(!debug.contains("synthetic-role-token"));
}

#[tokio::test(start_paused = true)]
async fn server_expiry_can_shorten_but_cannot_extend_the_one_hour_lease() {
    for (expiration, remaining) in [
        ("2024-01-01T00:05:00Z", 300),
        ("2024-01-01T02:00:00Z", 3600),
    ] {
        let http = replay(200, response(expiration));
        let connection = Connection::from_config(
            ConnectionOptions {
                role_arn: Some(ROLE.into()),
                ..options()
            },
            config(http),
        )
        .await
        .unwrap();
        tokio::time::advance(Duration::from_secs(remaining)).await;
        assert!(matches!(
            connection.session_status(),
            Err(Error::ExpiredRole)
        ));
    }
}

#[tokio::test]
async fn denied_or_already_expired_role_response_never_becomes_a_connection() {
    let http = replay(403, r#"<ErrorResponse><Error><Type>Sender</Type><Code>AccessDenied</Code><Message>not logged: synthetic-secret</Message></Error><RequestId>synthetic</RequestId></ErrorResponse>"#.into());
    let opts = ConnectionOptions {
        role_arn: Some(ROLE.into()),
        ..options()
    };
    let error = Connection::from_config(opts.clone(), config(http))
        .await
        .unwrap_err();
    assert!(matches!(&error, Error::AssumeRole { code } if code == "AccessDenied"));
    assert!(!format!("{error:?}").contains("synthetic-secret"));
    let http = replay(200, response("2023-12-31T23:59:00Z"));
    assert!(matches!(
        Connection::from_config(opts, config(http)).await,
        Err(Error::InvalidRoleCredentials)
    ));
}

#[test]
fn role_validation_accepts_other_accounts_and_partitions_but_rejects_wildcards() {
    for arn in [
        "arn:aws:iam::111111111111:role/team/reader",
        "arn:aws-us-gov:iam::222222222222:role/reader",
    ] {
        ConnectionOptions {
            role_arn: Some(arn.into()),
            ..options()
        }
        .validate()
        .unwrap();
    }
    for arn in [
        "arn:aws:iam::000000000000:role/*",
        "arn:aws:sts::000000000000:assumed-role/reader/session",
        "",
    ] {
        assert!(
            ConnectionOptions {
                role_arn: Some(arn.into()),
                ..options()
            }
            .validate()
            .is_err()
        );
    }
}

fn json_replay(responses: &[(u16, &str)]) -> StaticReplayClient {
    StaticReplayClient::new(
        responses
            .iter()
            .map(|(status, body)| {
                ReplayEvent::new(
                    http::Request::new(SdkBody::empty()),
                    http::Response::builder()
                        .status(*status)
                        .header("content-type", "application/x-amz-json-1.1")
                        .body(SdkBody::from(*body))
                        .unwrap(),
                )
            })
            .collect(),
    )
}

#[tokio::test]
async fn catalog_discovery_preserves_types_and_paginates_without_submitting_queries() {
    let http = json_replay(&[
        (
            200,
            r#"{"TableMetadata":{"Name":"types","TableType":"EXTERNAL_TABLE","Columns":[{"Name":"big","Type":"bigint","Comment":"invented"},{"Name":"future_type"}],"PartitionKeys":[{"Name":"day","Type":"date"}]}}"#,
        ),
        (
            200,
            r#"{"TableMetadataList":[{"Name":"types"}],"NextToken":"synthetic-next"}"#,
        ),
        (200, r#"{"TableMetadataList":[{"Name":"other"}]}"#),
    ]);
    let connection = Connection::from_config(options(), config(http.clone()))
        .await
        .unwrap();
    let table = connection.table_metadata("types").await.unwrap();
    assert_eq!(table.columns[0].data_type.as_deref(), Some("bigint"));
    assert_eq!(table.columns[0].comment.as_deref(), Some("invented"));
    assert_eq!(
        table.columns[1].data_type, None,
        "missing metadata must not invent a type"
    );
    assert_eq!(table.partition_keys[0].name, "day");
    let first = connection.table_page(None).await.unwrap();
    let second = connection
        .table_page(first.next_token.as_deref())
        .await
        .unwrap();
    assert_eq!(first.tables[0].name, "types");
    assert_eq!(second.tables[0].name, "other");
    assert_eq!(second.next_token, None);
    let requests: Vec<_> = http
        .actual_requests()
        .map(|request| {
            (
                request.headers().get("x-amz-target").unwrap().to_owned(),
                String::from_utf8_lossy(request.body().bytes().unwrap()).into_owned(),
            )
        })
        .collect();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].0.ends_with("GetTableMetadata"));
    assert!(requests[1].0.ends_with("ListTableMetadata"));
    assert!(requests[2].0.ends_with("ListTableMetadata"));
    assert!(!requests.iter().any(|(target, _)| target.contains("Query")));
    assert!(
        requests
            .iter()
            .all(|(_, body)| body.contains("synthetic_schema")
                && body.contains("synthetic-workgroup"))
    );
    assert!(requests[1].1.contains("\"MaxResults\":50"));
    assert!(requests[2].1.contains("\"NextToken\":\"synthetic-next\""));
}

#[tokio::test]
async fn catalog_failure_reports_code_without_logging_response_messages() {
    let http = json_replay(&[(
        400,
        r#"{"__type":"AccessDeniedException","Message":"not logged: synthetic-private-message"}"#,
    )]);
    let connection = Connection::from_config(options(), config(http))
        .await
        .unwrap();
    let error = connection.table_metadata("types").await.unwrap_err();
    assert!(
        matches!(&error, Error::Service { operation: "GetTableMetadata", code } if code == "AccessDeniedException")
    );
    assert!(!format!("{error:?}").contains("synthetic-private-message"));
    let http = json_replay(&[(200, "{}")]);
    let connection = Connection::from_config(options(), config(http))
        .await
        .unwrap();
    assert!(matches!(
        connection.table_metadata("types").await,
        Err(Error::InvalidServiceResponse("GetTableMetadata"))
    ));
}

#[tokio::test(start_paused = true)]
async fn expired_connection_rejects_catalog_calls_before_any_metadata_request() {
    let http = replay(200, response("2024-01-01T01:00:00Z"));
    let connection = Connection::from_config(
        ConnectionOptions {
            role_arn: Some(ROLE.into()),
            ..options()
        },
        config(http.clone()),
    )
    .await
    .unwrap();
    tokio::time::advance(ROLE_LIFETIME).await;
    assert!(matches!(
        connection.table_metadata("types").await,
        Err(Error::ExpiredRole)
    ));
    assert!(matches!(
        connection.table_page(None).await,
        Err(Error::ExpiredRole)
    ));
    assert_eq!(
        http.actual_requests().count(),
        1,
        "only the original role assumption was permitted"
    );
}
