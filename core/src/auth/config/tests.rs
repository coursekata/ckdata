use super::*;
use aws_credential_types::{Credentials, provider::ProvideCredentials};
use aws_runtime::env_config::file::EnvConfigFileKind;
use aws_smithy_http_client::test_util::NeverClient;

const PROFILES: &str = "[profile plain]\nsso_session = company\nsso_account_id = 000000000000\nsso_role_name = Reader\n[sso-session company]\nsso_start_url = https://synthetic.awsapps.com/start\nsso_region = us-west-1\n[profile chain]\nsource_profile = plain\nrole_arn = arn:aws:iam::000000000000:role/reader\n[profile static]\naws_access_key_id = PROFILE_KEY\naws_secret_access_key = invented\n[profile static-chain]\nsource_profile = static\nrole_arn = arn:aws:iam::000000000000:role/reader\n";

async fn resolve_options(options: &ConnectionOptions) -> Result<ConfiguredAuth, Error> {
    let config = SdkConfig::builder()
        .behavior_version(aws_config::BehaviorVersion::latest())
        .region(Region::new("us-west-2"))
        .credentials_provider(SharedCredentialsProvider::new(Credentials::new(
            "UNRELATED_ENV_KEY",
            "invented",
            None,
            None,
            "synthetic",
        )))
        .http_client(NeverClient::new())
        .build();
    resolve_with(options, config).await
}

async fn resolve_with(
    options: &ConnectionOptions,
    config: SdkConfig,
) -> Result<ConfiguredAuth, Error> {
    let files = EnvConfigFiles::builder()
        .with_contents(EnvConfigFileKind::Config, PROFILES)
        .build();
    resolve(
        options,
        config,
        files,
        &Fs::from_slice(&[]),
        &Env::from_slice(&[("AWS_PROFILE", "chain")]),
    )
    .await
}

fn direct() -> SsoOptions {
    SsoOptions {
        start_url: Some("https://direct.awsapps.com/start".into()),
        region: Some("us-east-1".into()),
        account_id: Some("111122223333".into()),
        role_name: Some("Reader".into()),
        ..Default::default()
    }
}

#[tokio::test]
async fn named_profile_pins_its_provider_and_missing_profile_never_uses_ambient_keys() {
    let mut options = ConnectionOptions::new("synthetic", "schema");
    options.profile = Some("static".into());
    let auth = resolve_options(&options).await.unwrap();
    assert_eq!(
        auth.config
            .credentials_provider()
            .unwrap()
            .provide_credentials()
            .await
            .unwrap()
            .access_key_id(),
        "PROFILE_KEY"
    );
    options.profile = Some("missing".into());
    assert!(matches!(
        resolve_options(&options).await,
        Err(Error::InvalidSetting("profile"))
    ));
}

#[tokio::test]
async fn complete_direct_sso_ignores_ambient_profiles_and_their_role_chains() {
    let mut options = ConnectionOptions::new("synthetic", "schema");
    options.sso = Some(SsoOptions {
        fallback_profile: Some("missing".into()),
        ..direct()
    });
    let auth = resolve_options(&options).await.unwrap();
    assert_eq!(auth.role_binding, None);
    let config = auth.target.unwrap().config.unwrap();
    assert!(config.contains("sso_account_id = 111122223333"));
    assert!(!config.contains("source_profile"));
    assert!(!config.contains("role_arn"));
}

#[tokio::test]
async fn plain_profile_fills_missing_fields_and_arguments_override_only_the_named_field() {
    let mut options = ConnectionOptions::new("synthetic", "schema");
    options.profile = Some("plain".into());
    options.sso = Some(SsoOptions {
        role_name: Some("Restricted".into()),
        ..Default::default()
    });
    let target = resolve_options(&options).await.unwrap().target.unwrap();
    assert_eq!(target.session.as_deref(), Some("company"));
    let config = target.config.unwrap();
    assert!(config.contains("sso_role_name = Restricted"));
    assert!(config.contains("sso_account_id = 000000000000"));
    assert!(config.contains("sso_region = us-west-1"));
    assert!(config.contains("sso_start_url = https://synthetic.awsapps.com/start"));
    options.sso.as_mut().unwrap().registration_scopes = Some("sso:account:access".into());
    assert_eq!(
        resolve_options(&options)
            .await
            .unwrap()
            .target
            .unwrap()
            .session
            .as_deref(),
        Some("company")
    );
}

#[tokio::test]
async fn chained_profiles_work_unchanged_but_sso_overrides_cannot_rewrite_them() {
    let mut options = ConnectionOptions::new("synthetic", "schema");
    options.profile = Some("chain".into());
    let auth = resolve_options(&options).await.unwrap();
    assert_eq!(
        auth.role_binding.as_deref(),
        Some("arn:aws:iam::000000000000:role/reader")
    );
    assert_eq!(auth.target.unwrap().profile, "plain");
    options.sso = Some(direct());
    assert!(matches!(
        resolve_options(&options).await,
        Err(Error::InvalidSetting(_))
    ));
    options.profile = Some("static".into());
    assert!(matches!(
        resolve_options(&options).await,
        Err(Error::InvalidSetting(_))
    ));
}

#[tokio::test]
async fn incomplete_direct_sso_does_not_search_or_extract_a_source_from_an_ambient_chain() {
    let mut options = ConnectionOptions::new("synthetic", "schema");
    options.sso = Some(SsoOptions {
        role_name: Some("Reader".into()),
        ..Default::default()
    });
    assert!(matches!(
        resolve_options(&options).await,
        Err(Error::InvalidSetting(_))
    ));
    options.sso.as_mut().unwrap().fallback_profile = Some("plain".into());
    assert!(resolve_options(&options).await.is_ok());
}

#[test]
fn sessions_are_stable_across_permission_sets_but_change_with_the_portal_or_scopes() {
    let original = direct();
    let mut changed = original.clone();
    changed.role_name = Some("AnotherPermissionSet".into());
    assert_eq!(original.target().session, changed.target().session);
    changed.start_url = Some("https://another.awsapps.com/start".into());
    assert_ne!(original.target().session, changed.target().session);
    changed = original.clone();
    changed.registration_scopes = Some("sso:account:access,example:read".into());
    assert_ne!(original.target().session, changed.target().session);
    changed.session = Some("separate-human-login".into());
    assert_eq!(
        changed.target().session.as_deref(),
        Some("separate-human-login")
    );
}

#[test]
fn invalid_sso_values_cannot_inject_configuration_or_use_a_service_region_as_fallback() {
    let mut options = direct();
    options.region = None;
    assert!(options.validate().is_err());
    for value in ["Reader\nrole_arn = injected", "[profile injected]", ""] {
        let mut options = direct();
        options.role_name = Some(value.into());
        assert!(options.validate().is_err());
    }
}

#[tokio::test]
async fn a_profile_already_targeting_the_required_role_does_not_assume_it_a_second_time() {
    use aws_smithy_http_client::test_util::{ReplayEvent, StaticReplayClient};
    use aws_smithy_types::body::SdkBody;
    let response = r#"<AssumeRoleResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/"><AssumeRoleResult><Credentials><AccessKeyId>TARGET_KEY</AccessKeyId><SecretAccessKey>invented</SecretAccessKey><SessionToken>invented</SessionToken><Expiration>2030-01-01T01:00:00Z</Expiration></Credentials></AssumeRoleResult></AssumeRoleResponse>"#;
    let http = StaticReplayClient::new(vec![ReplayEvent::new(
        http::Request::new(SdkBody::empty()),
        http::Response::builder()
            .status(200)
            .header("content-type", "text/xml")
            .body(SdkBody::from(response))
            .unwrap(),
    )]);
    let config = SdkConfig::builder()
        .behavior_version(aws_config::BehaviorVersion::latest())
        .region(Region::new("us-west-2"))
        .http_client(http.clone())
        .build();
    let mut options = ConnectionOptions::new("synthetic", "schema");
    options.profile = Some("static-chain".into());
    options.role_arn = Some("arn:aws:iam::000000000000:role/reader".into());
    let auth = resolve_with(&options, config).await.unwrap();
    crate::Connection::from_auth(options, auth).await.unwrap();
    assert_eq!(http.actual_requests().count(), 1);
    assert!(
        http.actual_requests()
            .next()
            .unwrap()
            .headers()
            .get("authorization")
            .unwrap()
            .contains("PROFILE_KEY")
    );
}

#[tokio::test]
async fn a_profile_role_change_cannot_invalidate_the_proof_used_to_skip_extra_assumption() {
    let profile = BoundProfile {
        provider: SharedCredentialsProvider::new(Credentials::new(
            "UNRELATED",
            "invented",
            None,
            None,
            "synthetic",
        )),
        files: EnvConfigFiles::builder()
            .with_contents(EnvConfigFileKind::Config, PROFILES)
            .build(),
        name: "static-chain".into(),
        role: "arn:aws:iam::000000000000:role/changed".into(),
    };
    assert!(matches!(
        profile.provide_credentials().await,
        Err(CredentialsError::InvalidConfiguration(_))
    ));
}

#[tokio::test]
async fn a_direct_sso_override_keeps_the_selected_profiles_service_region_as_a_default() {
    let mut options = ConnectionOptions::new("synthetic", "schema");
    options.sso = Some(SsoOptions {
        fallback_profile: Some("configured".into()),
        ..direct()
    });
    let loader = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .env(Env::from_slice(&[
            ("AWS_CONFIG_FILE", "/synthetic/config"),
            ("AWS_SHARED_CREDENTIALS_FILE", "/synthetic/absent"),
            ("AWS_PROFILE", "unrelated"),
            ("AWS_EC2_METADATA_DISABLED", "true"),
        ]))
        .fs(Fs::from_slice(&[(
            "/synthetic/config",
            "[profile configured]\nregion = us-west-2\n[profile unrelated]\nregion = eu-west-1\n",
        )]))
        .http_client(NeverClient::new());
    let auth = configure(&options, loader).await.unwrap();
    assert_eq!(auth.config.region().unwrap().as_ref(), "us-west-2");
    assert!(
        auth.target
            .unwrap()
            .config
            .unwrap()
            .contains("sso_region = us-east-1")
    );
}
