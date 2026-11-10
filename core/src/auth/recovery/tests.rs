use super::*;
use aws_credential_types::{
    Credentials,
    provider::error::{CredentialsError, TokenError},
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

async fn within_test_deadline<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(2), future)
        .await
        .expect("synthetic credential recovery exceeded its test deadline")
}

#[derive(Debug)]
struct Expired;
impl ProvideCredentials for Expired {
    fn provide_credentials<'a>(&'a self) -> provider::future::ProvideCredentials<'a>
    where
        Self: 'a,
    {
        provider::future::ProvideCredentials::ready(Err(CredentialsError::provider_error(
            TokenError::provider_error("synthetic expired token"),
        )))
    }
}

fn target() -> SsoLoginTarget {
    SsoLoginTarget {
        profile: "synthetic".into(),
        session: Some("synthetic-session".into()),
        config: None,
    }
}

async fn wait_for_login(login: &SsoLogin) {
    within_test_deadline(async {
        while login.pending().is_none() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;
}

async fn answer(login: &SsoLogin, success: bool) {
    wait_for_login(login).await;
    login.complete(success);
}

#[tokio::test]
async fn reauthentication_rebuilds_only_the_selected_provider_and_uses_one_login_budget() {
    let builds = Arc::new(AtomicUsize::new(0));
    let counter = builds.clone();
    let factory: ProviderFactory = Arc::new(move || {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            SharedCredentialsProvider::new(Expired)
        } else {
            SharedCredentialsProvider::new(Credentials::new(
                "RENEWED",
                "invented",
                None,
                None,
                "synthetic",
            ))
        }
    });
    let provider = RecoveringCredentials::new(factory, Some(target()));
    let login = SsoLogin::default();
    let (credentials, ()) = within_test_deadline(async {
        tokio::join!(
            login.scope(provider.provide_credentials()),
            answer(&login, true)
        )
    })
    .await;
    assert_eq!(credentials.unwrap().access_key_id(), "RENEWED");
    assert_eq!(builds.load(Ordering::SeqCst), 2);
    assert!(login.pending().is_none());
    let always_expired = RecoveringCredentials::new(
        Arc::new(|| SharedCredentialsProvider::new(Expired)),
        Some(target()),
    );
    assert!(
        within_test_deadline(login.scope(always_expired.provide_credentials()))
            .await
            .is_err()
    );
    assert!(
        login.pending().is_none(),
        "a second provider layer must not launch another login"
    );
}

#[tokio::test]
async fn disabled_failed_and_unsuccessful_reauthentication_never_loop_or_use_other_credentials() {
    let builds = Arc::new(AtomicUsize::new(0));
    let counter = builds.clone();
    let provider = RecoveringCredentials::new(
        Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            SharedCredentialsProvider::new(Expired)
        }),
        Some(target()),
    );
    assert!(
        within_test_deadline(provider.provide_credentials())
            .await
            .is_err(),
        "no host scope means no login"
    );
    assert_eq!(builds.load(Ordering::SeqCst), 1);
    let disabled = SsoLogin::new(false);
    assert!(
        within_test_deadline(disabled.scope(provider.provide_credentials()))
            .await
            .is_err()
    );
    assert!(disabled.pending().is_none());
    assert_eq!(disabled.required(), Some(target()));
    let login = SsoLogin::default();
    let (result, ()) = within_test_deadline(async {
        tokio::join!(
            login.scope(provider.provide_credentials()),
            answer(&login, false)
        )
    })
    .await;
    assert!(result.is_err());
    assert_eq!(builds.load(Ordering::SeqCst), 1);
    let login = SsoLogin::default();
    let (result, ()) = within_test_deadline(async {
        tokio::join!(
            login.scope(provider.provide_credentials()),
            answer(&login, true)
        )
    })
    .await;
    assert!(result.is_err());
    assert_eq!(builds.load(Ordering::SeqCst), 2);
    assert!(login.pending().is_none());
}

#[tokio::test]
async fn valid_credentials_and_unrelated_errors_do_not_request_interaction() {
    let login = SsoLogin::default();
    let provider = RecoveringCredentials::new(
        Arc::new(|| {
            SharedCredentialsProvider::new(Credentials::new(
                "VALID",
                "invented",
                None,
                None,
                "synthetic",
            ))
        }),
        Some(target()),
    );
    assert_eq!(
        within_test_deadline(login.scope(provider.provide_credentials()))
            .await
            .unwrap()
            .access_key_id(),
        "VALID"
    );
    assert!(login.pending().is_none());
    #[derive(Debug)]
    struct Denied;
    impl ProvideCredentials for Denied {
        fn provide_credentials<'a>(&'a self) -> provider::future::ProvideCredentials<'a>
        where
            Self: 'a,
        {
            provider::future::ProvideCredentials::ready(Err(
                CredentialsError::invalid_configuration("synthetic AccessDenied"),
            ))
        }
    }
    let provider = RecoveringCredentials::new(
        Arc::new(|| SharedCredentialsProvider::new(Denied)),
        Some(target()),
    );
    assert!(
        within_test_deadline(login.scope(provider.provide_credentials()))
            .await
            .is_err()
    );
    assert!(login.pending().is_none());
}

#[tokio::test]
async fn a_cancelled_host_cannot_publish_a_late_login_request() {
    let login = SsoLogin::default();
    login.complete(false);
    let provider = RecoveringCredentials::new(
        Arc::new(|| SharedCredentialsProvider::new(Expired)),
        Some(target()),
    );
    assert!(
        within_test_deadline(login.scope(provider.provide_credentials()))
            .await
            .is_err()
    );
    assert!(login.pending().is_none());
    assert_eq!(login.required(), Some(target()));
}

#[tokio::test]
async fn sdk_signing_waits_for_host_login_then_sends_only_with_recovered_credentials() {
    use aws_smithy_http_client::test_util::infallible_client_fn;
    use aws_types::{SdkConfig, region::Region};
    let builds = Arc::new(AtomicUsize::new(0));
    let counter = builds.clone();
    let provider = RecoveringCredentials::new(
        Arc::new(move || {
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                SharedCredentialsProvider::new(Expired)
            } else {
                SharedCredentialsProvider::new(Credentials::new(
                    "RECOVERED_KEY",
                    "invented",
                    None,
                    None,
                    "synthetic",
                ))
            }
        }),
        Some(target()),
    );
    let requests = Arc::new(AtomicUsize::new(0));
    let observed = requests.clone();
    let config = SdkConfig::builder()
        .behavior_version(aws_config::BehaviorVersion::latest())
        .region(Region::new("us-west-2"))
        .credentials_provider(SharedCredentialsProvider::new(provider))
        .http_client(infallible_client_fn(move |request| {
            observed.fetch_add(1, Ordering::SeqCst);
            assert!(
                request.headers()["authorization"]
                    .to_str()
                    .unwrap()
                    .contains("RECOVERED_KEY")
            );
            http::Response::builder()
                .status(200)
                .header("content-type", "application/x-amz-json-1.1")
                .body(r#"{"WorkGroup":{"Name":"synthetic"}}"#)
                .unwrap()
        }))
        .build();
    let client = aws_sdk_athena::Client::new(&config);
    let login = SsoLogin::default();
    let request = client.get_work_group().work_group("synthetic");
    let (result, ()) = tokio::time::timeout(
        Duration::from_secs(2),
        login.scope(async { tokio::join!(request.send(), answer(&login, true)) }),
    )
    .await
    .expect("SDK credential signing must expose the login request to its host");
    assert_eq!(result.unwrap().work_group().unwrap().name(), "synthetic");
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(builds.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn interactive_sdk_recovery_allows_browser_time_instead_of_the_short_request_timeout() {
    use aws_smithy_http_client::test_util::infallible_client_fn;
    use aws_types::{SdkConfig, region::Region};
    #[derive(Debug)]
    struct InitiallyValid(AtomicUsize);
    impl ProvideCredentials for InitiallyValid {
        fn provide_credentials<'a>(&'a self) -> provider::future::ProvideCredentials<'a>
        where
            Self: 'a,
        {
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                provider::future::ProvideCredentials::ready(Ok(Credentials::new(
                    "INITIAL",
                    "invented",
                    None,
                    None,
                    "synthetic",
                )))
            } else {
                Expired.provide_credentials()
            }
        }
    }
    let builds = Arc::new(AtomicUsize::new(0));
    let observed = builds.clone();
    let provider = RecoveringCredentials::new(
        Arc::new(move || {
            if observed.fetch_add(1, Ordering::SeqCst) == 0 {
                SharedCredentialsProvider::new(InitiallyValid(AtomicUsize::new(0)))
            } else {
                SharedCredentialsProvider::new(Credentials::new(
                    "RENEWED",
                    "invented",
                    None,
                    None,
                    "synthetic",
                ))
            }
        }),
        Some(target()),
    );
    let config = SdkConfig::builder()
        .behavior_version(aws_config::BehaviorVersion::latest())
        .region(Region::new("us-west-2"))
        .credentials_provider(SharedCredentialsProvider::new(provider))
        .http_client(infallible_client_fn(|request| {
            assert!(
                request.headers()["authorization"]
                    .to_str()
                    .unwrap()
                    .contains("RENEWED")
            );
            http::Response::builder()
                .status(200)
                .header("content-type", "application/x-amz-json-1.1")
                .body(r#"{"WorkGroup":{"Name":"synthetic","State":"ENABLED","Configuration":{}}}"#)
                .unwrap()
        }))
        .build();
    let login = SsoLogin::default();
    tokio::time::timeout(
        Duration::from_secs(30),
        login.scope(async {
            let connection = crate::Connection::from_auth(
                crate::ConnectionOptions::new("synthetic", "schema"),
                crate::auth::ConfiguredAuth {
                    config,
                    target: Some(target()),
                    role_binding: None,
                },
            )
            .await
            .unwrap();
            let (result, ()) = tokio::join!(connection.workgroup_configuration(), async {
                wait_for_login(&login).await;
                tokio::time::sleep(Duration::from_secs(15)).await;
                login.complete(true);
            });
            result.unwrap();
        }),
    )
    .await
    .expect("interactive recovery must finish within the synthetic browser deadline");
    assert_eq!(builds.load(Ordering::SeqCst), 2);
}
