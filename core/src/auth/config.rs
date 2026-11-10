use std::sync::Arc;

use aws_config::{
    ConfigLoader, profile::ProfileFileCredentialsProvider, provider_config::ProviderConfig,
};
use aws_credential_types::provider::SharedCredentialsProvider;
use aws_credential_types::provider::{self, ProvideCredentials, error::CredentialsError};
use aws_types::{SdkConfig, region::Region};
use sha2::{Digest, Sha256};

use super::{Env, EnvConfigFiles, Fs, ProfileSet, SsoLoginTarget, profile_login_target};
use crate::{ConnectionOptions, Error};

/// Independent SSO settings. A fallback profile supplies missing fields only;
/// it never contributes role assumptions to direct SSO authentication.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SsoOptions {
    pub start_url: Option<String>,
    pub region: Option<String>,
    pub account_id: Option<String>,
    pub role_name: Option<String>,
    pub session: Option<String>,
    pub registration_scopes: Option<String>,
    pub fallback_profile: Option<String>,
}

impl SsoOptions {
    fn complete(&self) -> bool {
        [
            &self.start_url,
            &self.region,
            &self.account_id,
            &self.role_name,
        ]
        .iter()
        .all(|value| value.is_some())
    }

    fn fill_from(&mut self, profiles: &ProfileSet, name: &str) -> Result<(), Error> {
        let profile = profiles
            .get_profile(name)
            .ok_or(Error::InvalidSetting("SSO profile"))?;
        let has_sso_settings = [
            "sso_session",
            "sso_start_url",
            "sso_region",
            "sso_account_id",
            "sso_role_name",
        ]
        .iter()
        .any(|field| profile.get(field).is_some());
        if !has_sso_settings
            || [
                "role_arn",
                "credential_source",
                "web_identity_token_file",
                "credential_process",
            ]
            .iter()
            .any(|field| profile.get(field).is_some())
        {
            return Err(Error::InvalidSetting(
                "SSO overrides require a plain SSO profile; select the configured profile unchanged or supply direct SSO without a profile",
            ));
        }
        let session = profile
            .get("sso_session")
            .and_then(|name| profiles.sso_session(name));
        let setting = |key| {
            session
                .and_then(|session| session.get(key))
                .or_else(|| profile.get(key))
                .map(str::to_owned)
        };
        let inherited_url = setting("sso_start_url");
        let inherited_region = setting("sso_region");
        let inherited_scopes = setting("sso_registration_scopes");
        let effective_scopes = Some(
            inherited_scopes
                .clone()
                .unwrap_or_else(|| "sso:account:access".into()),
        );
        // A changed portal/region/scope must not overwrite an unrelated named
        // session. An explicitly requested session remains the caller's choice.
        let compatible = [
            (&self.start_url, &inherited_url),
            (&self.region, &inherited_region),
            (&self.registration_scopes, &effective_scopes),
        ]
        .iter()
        .all(|(value, inherited)| value.is_none() || value == inherited);
        if compatible && self.session.is_none() {
            self.session = profile.get("sso_session").map(str::to_owned);
        }
        self.start_url = self.start_url.take().or(inherited_url);
        self.region = self.region.take().or(inherited_region);
        self.registration_scopes = self.registration_scopes.take().or(inherited_scopes);
        self.account_id = self
            .account_id
            .take()
            .or_else(|| profile.get("sso_account_id").map(str::to_owned));
        self.role_name = self
            .role_name
            .take()
            .or_else(|| profile.get("sso_role_name").map(str::to_owned));
        Ok(())
    }

    fn validate(&self) -> Result<(), Error> {
        for (name, value) in [
            ("sso_start_url", &self.start_url),
            ("sso_region", &self.region),
            ("sso_account_id", &self.account_id),
            ("sso_role_name", &self.role_name),
            ("sso_session", &self.session),
            ("sso_registration_scopes", &self.registration_scopes),
        ] {
            if value.as_ref().is_some_and(|value| {
                value.trim().is_empty()
                    || value.chars().any(char::is_control)
                    || value.contains(['[', ']'])
            }) {
                return Err(Error::InvalidSetting(name));
            }
        }
        if !self.complete() {
            return Err(Error::InvalidSetting(
                "SSO requires sso_start_url, sso_region, sso_account_id and sso_role_name",
            ));
        }
        if !self.start_url.as_ref().unwrap().starts_with("https://") {
            return Err(Error::InvalidSetting("sso_start_url must use https"));
        }
        let account = self.account_id.as_ref().unwrap();
        if account.len() != 12 || !account.bytes().all(|ch| ch.is_ascii_digit()) {
            return Err(Error::InvalidSetting("sso_account_id"));
        }
        Ok(())
    }

    fn target(&self) -> SsoLoginTarget {
        let scopes = self
            .registration_scopes
            .as_deref()
            .unwrap_or("sso:account:access");
        let session = self.session.clone().unwrap_or_else(|| {
            let identity = format!(
                "{}\n{}\n{scopes}",
                self.start_url.as_deref().unwrap(),
                self.region.as_deref().unwrap()
            );
            format!(
                "ckdata-{}",
                Sha256::digest(identity.as_bytes())
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            )
        });
        let profile = "ckdata-sso".to_owned();
        let config = format!(
            "[profile {profile}]\nsso_session = {session}\nsso_account_id = {}\nsso_role_name = {}\n[sso-session {session}]\nsso_start_url = {}\nsso_region = {}\nsso_registration_scopes = {scopes}\n",
            self.account_id.as_deref().unwrap(),
            self.role_name.as_deref().unwrap(),
            self.start_url.as_deref().unwrap(),
            self.region.as_deref().unwrap(),
        );
        SsoLoginTarget {
            profile,
            session: Some(session),
            config: Some(config),
        }
    }
}

pub(crate) struct ConfiguredAuth {
    pub config: SdkConfig,
    pub target: Option<SsoLoginTarget>,
    pub role_binding: Option<String>,
}

#[derive(Debug)]
struct BoundProfile {
    provider: SharedCredentialsProvider,
    files: EnvConfigFiles,
    name: String,
    role: String,
}

#[derive(Debug)]
struct DefaultSource {
    providers: ProviderConfig,
    profile: String,
}

impl ProvideCredentials for DefaultSource {
    fn provide_credentials<'a>(&'a self) -> provider::future::ProvideCredentials<'a>
    where
        Self: 'a,
    {
        provider::future::ProvideCredentials::new(async move {
            aws_config::default_provider::credentials::DefaultCredentialsChain::builder()
                .configure(self.providers.clone())
                .profile_name(&self.profile)
                .build()
                .await
                .provide_credentials()
                .await
        })
    }
}

impl ProvideCredentials for BoundProfile {
    fn provide_credentials<'a>(&'a self) -> provider::future::ProvideCredentials<'a>
    where
        Self: 'a,
    {
        provider::future::ProvideCredentials::new(async move {
            let profiles = aws_config::profile::load(
                &Fs::real(),
                &Env::real(),
                &self.files,
                Some(self.name.clone().into()),
            )
            .await
            .map_err(|_| CredentialsError::invalid_configuration("profile could not be loaded"))?;
            if profiles
                .get_profile(&self.name)
                .and_then(|profile| profile.get("role_arn"))
                != Some(self.role.as_str())
            {
                return Err(CredentialsError::invalid_configuration(
                    "profile role changed; reconnect to select new authentication settings",
                ));
            }
            self.provider.provide_credentials().await
        })
    }
}

pub(crate) async fn configure(
    options: &ConnectionOptions,
    mut loader: ConfigLoader,
) -> Result<ConfiguredAuth, Error> {
    options.validate()?;
    if let Some(profile) = options.profile.as_ref().or_else(|| {
        options
            .sso
            .as_ref()
            .and_then(|sso| sso.fallback_profile.as_ref())
    }) {
        loader = loader.profile_name(profile);
    }
    if let Some(region) = &options.region {
        loader = loader.region(Region::new(region.clone()));
    }
    let config = loader.load().await;
    resolve(
        options,
        config,
        EnvConfigFiles::default(),
        &Fs::real(),
        &Env::real(),
    )
    .await
}

pub(super) async fn resolve(
    options: &ConnectionOptions,
    config: SdkConfig,
    files: EnvConfigFiles,
    fs: &Fs,
    env: &Env,
) -> Result<ConfiguredAuth, Error> {
    let complete_direct =
        options.profile.is_none() && options.sso.as_ref().is_some_and(SsoOptions::complete);
    let selected = options.profile.as_deref().or_else(|| {
        options
            .sso
            .as_ref()
            .and_then(|sso| sso.fallback_profile.as_deref())
    });
    let profiles = if complete_direct {
        None
    } else {
        aws_config::profile::load(fs, env, &files, selected.map(|name| name.to_owned().into()))
            .await
            .ok()
    };
    let providers = provider_config(&config);
    let source = if options.sso.is_some() {
        sso_source(options, profiles.as_ref(), providers)?
    } else if let Some(name) = &options.profile {
        profile_source(name, profiles.as_ref(), providers, files)?
    } else {
        let profile = profiles
            .as_ref()
            .map_or("default", |profiles| profiles.selected_profile())
            .to_owned();
        Source {
            factory: Arc::new(move || {
                SharedCredentialsProvider::new(DefaultSource {
                    providers: providers.clone(),
                    profile: profile.clone(),
                })
            }),
            target: profiles.as_ref().and_then(profile_login_target),
            role_binding: None,
        }
    };
    let Source {
        factory,
        target,
        role_binding,
    } = source;
    let provider = super::recovery::RecoveringCredentials::new(factory, target.clone());
    Ok(ConfiguredAuth {
        config: config
            .to_builder()
            .credentials_provider(SharedCredentialsProvider::new(provider))
            .build(),
        target,
        role_binding,
    })
}

struct Source {
    factory: super::recovery::ProviderFactory,
    target: Option<SsoLoginTarget>,
    role_binding: Option<String>,
}

fn provider_config(config: &SdkConfig) -> ProviderConfig {
    let mut providers = ProviderConfig::without_region()
        .with_region(config.region().cloned())
        .with_use_fips(config.use_fips())
        .with_use_dual_stack(config.use_dual_stack());
    if let Some(http) = config.http_client() {
        providers = providers.with_http_client(http.clone());
    }
    if let Some(clock) = config.time_source() {
        providers = providers.with_time_source(clock.clone());
    }
    if let Some(retry) = config.retry_config() {
        providers = providers.with_retry_config(retry.clone());
    }
    if let Some(sleep) = config.sleep_impl() {
        providers = providers.with_sleep_impl(sleep);
    }
    if let Some(timeout) = config.timeout_config() {
        providers = providers.with_timeout_config(timeout.clone());
    }
    providers
}

fn sso_source(
    options: &ConnectionOptions,
    profiles: Option<&ProfileSet>,
    providers: ProviderConfig,
) -> Result<Source, Error> {
    let mut sso = options.sso.clone().expect("SSO source selected");
    if options.profile.is_some() || !sso.complete() {
        let profiles = profiles.ok_or(Error::InvalidSetting(
            "SSO settings are incomplete and no plain SSO fallback profile is available",
        ))?;
        sso.fill_from(profiles, profiles.selected_profile())?;
    }
    sso.validate()?;
    let target = sso.target();
    let session = target.session.clone().unwrap();
    let factory = Arc::new(move || {
        SharedCredentialsProvider::new(
            aws_config::sso::SsoCredentialsProvider::builder()
                .configure(&providers)
                .start_url(sso.start_url.as_ref().unwrap())
                .region(Region::new(sso.region.clone().unwrap()))
                .account_id(sso.account_id.as_ref().unwrap())
                .role_name(sso.role_name.as_ref().unwrap())
                .session_name(&session)
                .build(),
        )
    });
    Ok(Source {
        factory,
        target: Some(target),
        role_binding: None,
    })
}

fn profile_source(
    name: &str,
    profiles: Option<&ProfileSet>,
    providers: ProviderConfig,
    files: EnvConfigFiles,
) -> Result<Source, Error> {
    let profiles = profiles
        .filter(|set| set.get_profile(name).is_some())
        .ok_or(Error::InvalidSetting("profile"))?;
    let binding = profiles
        .get_profile(name)
        .and_then(|profile| profile.get("role_arn"))
        .map(str::to_owned);
    let target = profile_login_target(profiles);
    let name = name.to_owned();
    let role = binding.clone();
    let factory = Arc::new(move || {
        let provider = SharedCredentialsProvider::new(
            ProfileFileCredentialsProvider::builder()
                .configure(&providers)
                .profile_name(&name)
                .profile_files(files.clone())
                .build(),
        );
        if let Some(role) = &role {
            SharedCredentialsProvider::new(BoundProfile {
                provider,
                files: files.clone(),
                name: name.clone(),
                role: role.clone(),
            })
        } else {
            provider
        }
    });
    Ok(Source {
        factory,
        target,
        role_binding: binding,
    })
}

#[cfg(test)]
mod tests;
