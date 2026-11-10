use super::*;
use aws_credential_types::provider::ProvideCredentials;

const CONFIG: &str = "[profile reader]\nrole_arn = arn:aws:iam::000000000000:role/test\nsource_profile = workforce\n[profile workforce]\nsso_session = synthetic\nsso_account_id = 000000000000\nsso_role_name = Reader\n[sso-session synthetic]\nsso_start_url = https://synthetic.awsapps.com/start\nsso_region = us-west-2\n";

async fn profiles(config: &str, selected: &str) -> ProfileSet {
    aws_config::profile::load(
        &Fs::from_slice(&[("/synthetic/config", config)]),
        &Env::from_slice(&[
            ("AWS_CONFIG_FILE", "/synthetic/config"),
            ("AWS_SHARED_CREDENTIALS_FILE", "/synthetic/absent"),
            ("AWS_PROFILE", selected),
        ]),
        &EnvConfigFiles::default(),
        None,
    )
    .await
    .unwrap()
}

fn target() -> SsoLoginTarget {
    SsoLoginTarget {
        profile: "workforce".to_owned(),
        session: Some("synthetic".to_owned()),
        config: None,
    }
}

async fn sdk_config(selected: &str, fs: Fs) -> aws_types::SdkConfig {
    aws_config::defaults(aws_config::BehaviorVersion::latest())
        .env(Env::from_slice(&[
            ("AWS_CONFIG_FILE", "/synthetic/config"),
            ("AWS_SHARED_CREDENTIALS_FILE", "/synthetic/absent"),
            ("AWS_PROFILE", selected),
            ("HOME", "/synthetic/home"),
            ("USERPROFILE", "/synthetic/home"),
            ("AWS_EC2_METADATA_DISABLED", "true"),
        ]))
        .fs(fs)
        .region(aws_types::region::Region::new("us-west-2"))
        .http_client(aws_smithy_http_client::test_util::NeverClient::new())
        .load()
        .await
}

#[tokio::test]
async fn login_targets_match_the_sdk_for_mixed_and_self_referencing_profiles() {
    for setting in [
        "credential_process = synthetic-unused-command",
        "aws_access_key_id = SYNTHETIC\naws_secret_access_key = synthetic-secret",
        "source_profile = ignored-without-role",
        "role_arn = arn:aws:iam::000000000000:role/test\nsource_profile = workforce",
    ] {
        let config = CONFIG.replace(
            "sso_session = synthetic",
            &format!("{setting}\nsso_session = synthetic"),
        );
        let sdk = sdk_config(
            "workforce",
            Fs::from_slice(&[("/synthetic/config", &config)]),
        )
        .await;
        let error = sdk
            .credentials_provider()
            .unwrap()
            .provide_credentials()
            .await
            .unwrap_err();
        // Establish that the SDK actually chose SSO, not an unused alternative.
        assert!(
            std::iter::successors(Some(&error as &(dyn StdError + 'static)), |cause| (*cause)
                .source())
            .any(|cause| cause.is::<TokenError>()),
            "{setting}"
        );
        assert_eq!(
            profile_login_target(&profiles(&config, "workforce").await),
            Some(target()),
            "{setting}"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn sdk_wrapped_cache_filesystem_errors_do_not_request_login() {
    struct SyntheticFiles(std::path::PathBuf);
    impl Drop for SyntheticFiles {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let root = std::env::temp_dir().join(format!("ckdata-auth-fs-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let files = SyntheticFiles(root);
    std::fs::write(files.0.join("config"), CONFIG).unwrap();
    // A file instead of the synthetic home directory deterministically produces
    // a filesystem error even for privileged test runners. No tokens are made.
    std::fs::write(files.0.join("home"), "invented non-directory").unwrap();
    let sdk = sdk_config("reader", Fs::from_test_dir(&files.0, "/synthetic")).await;
    let error = sdk
        .credentials_provider()
        .unwrap()
        .provide_credentials()
        .await
        .unwrap_err();
    assert!(matches!(
        credential_error(error, Some(target())),
        Error::CredentialsUnavailable
    ));
}

#[tokio::test]
async fn sdk_profiles_resolve_the_sso_source_of_a_role_chain() {
    assert_eq!(
        profile_login_target(&profiles(CONFIG, "reader").await),
        Some(target())
    );
    assert_eq!(
        profile_login_target(&profiles(CONFIG, "missing").await),
        None
    );
    let legacy = "[default]\nsso_start_url = https://synthetic.awsapps.com/start\nsso_region = us-west-2\nsso_account_id = 000000000000\nsso_role_name = Reader\n";
    assert_eq!(
        profile_login_target(&profiles(legacy, "default").await),
        Some(SsoLoginTarget {
            profile: "default".into(),
            session: None,
            config: None,
        })
    );
}

#[tokio::test]
async fn alternate_authentication_incomplete_profiles_and_cycles_do_not_offer_sso() {
    for setting in [
        "credential_process = synthetic-command",
        "aws_access_key_id = synthetic-key",
        "web_identity_token_file = /synthetic/token",
        "credential_source = EcsContainer",
        "source_profile = reader",
    ] {
        let config = CONFIG.replace("source_profile = workforce", setting);
        assert_eq!(
            profile_login_target(&profiles(&config, "reader").await),
            None
        );
    }
    let config = CONFIG.replace("sso_region = us-west-2", "");
    assert_eq!(
        profile_login_target(&profiles(&config, "reader").await),
        None
    );
    let static_source = CONFIG.replace("sso_session = synthetic", "aws_access_key_id = SYNTHETIC\naws_secret_access_key = synthetic-secret\nsso_session = synthetic");
    assert_eq!(
        profile_login_target(&profiles(&static_source, "reader").await),
        None
    );
    let cycle = CONFIG.replace("sso_session = synthetic", "role_arn = arn:aws:iam::000000000000:role/test\nsource_profile = reader\nsso_session = synthetic");
    assert_eq!(
        profile_login_target(&profiles(&cycle, "reader").await),
        None
    );
}

#[tokio::test]
async fn missing_sdk_sso_cache_is_a_typed_login_error_without_http() {
    let config = sdk_config("reader", Fs::from_slice(&[("/synthetic/config", CONFIG)])).await;
    let error = config
        .credentials_provider()
        .unwrap()
        .provide_credentials()
        .await
        .unwrap_err();
    let error = credential_error(error, Some(target()));
    assert!(matches!(error, Error::SsoLoginRequired { target: actual } if actual == target()));
}

#[tokio::test]
async fn expired_sdk_sso_cache_requests_login_through_multiple_role_hops() {
    let config = format!(
        "[profile outer]\nrole_arn = arn:aws:iam::000000000000:role/outer\nsource_profile = reader\n{CONFIG}"
    );
    // SHA-1 of the invented session name `synthetic`, as used by AWS's cache.
    let cache = "/synthetic/home/.aws/sso/cache/385fcd6760f6432fcdc51c5f995ea61a31d5bbab.json";
    let expired = r#"{"accessToken":"invented-expired-token","expiresAt":"2000-01-01T00:00:00Z"}"#;
    let sdk = sdk_config(
        "outer",
        Fs::from_slice(&[("/synthetic/config", config.as_str()), (cache, expired)]),
    )
    .await;
    let error = sdk
        .credentials_provider()
        .unwrap()
        .provide_credentials()
        .await
        .unwrap_err();
    assert!(matches!(
        credential_error(error, profile_login_target(&profiles(&config, "outer").await)),
        Error::SsoLoginRequired { target: actual } if actual == target()
    ));
}

#[test]
fn token_failures_require_sso_but_transport_and_configuration_errors_do_not() {
    let expired =
        CredentialsError::provider_error(TokenError::provider_error("synthetic expired token"));
    assert!(matches!(
        credential_error(expired, Some(target())),
        Error::SsoLoginRequired { .. }
    ));
    for kind in [
        std::io::ErrorKind::ConnectionRefused,
        std::io::ErrorKind::PermissionDenied,
    ] {
        let error =
            CredentialsError::provider_error(std::io::Error::new(kind, "synthetic failure"));
        assert!(matches!(
            credential_error(error, Some(target())),
            Error::CredentialsUnavailable
        ));
    }
    let no_profile =
        CredentialsError::provider_error(std::io::Error::from(std::io::ErrorKind::NotFound));
    assert!(matches!(
        credential_error(no_profile, None),
        Error::CredentialsUnavailable
    ));
    for token in [
        TokenError::invalid_configuration("synthetic invalid configuration"),
        TokenError::provider_timed_out(std::time::Duration::from_secs(1)),
        TokenError::not_loaded("no configured provider"),
        TokenError::unhandled("synthetic malformed response"),
        TokenError::provider_error(std::io::Error::from(std::io::ErrorKind::ConnectionRefused)),
    ] {
        assert!(matches!(
            credential_error(CredentialsError::provider_error(token), Some(target())),
            Error::CredentialsUnavailable
        ));
    }
}

#[test]
fn rejected_sso_tokens_offer_login_but_missing_role_assignments_do_not() {
    use aws_sdk_sso::types::error::{ResourceNotFoundException, UnauthorizedException};
    let cases = [
        (
            GetRoleCredentialsError::UnauthorizedException(
                UnauthorizedException::builder().build(),
            ),
            true,
        ),
        (
            GetRoleCredentialsError::ResourceNotFoundException(
                ResourceNotFoundException::builder().build(),
            ),
            false,
        ),
    ];
    for (error, expected) in cases {
        let response = aws_sdk_sso::config::http::HttpResponse::new(
            401.try_into().unwrap(),
            aws_smithy_types::body::SdkBody::empty(),
        );
        let sdk = aws_sdk_sso::error::SdkError::service_error(error, response);
        let error = credential_error(CredentialsError::provider_error(sdk), Some(target()));
        assert_eq!(matches!(error, Error::SsoLoginRequired { .. }), expected);
    }
}
