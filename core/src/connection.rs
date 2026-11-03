use std::time::{Duration, SystemTime};

use aws_config::identity::IdentityCache;
use aws_config::{BehaviorVersion, ConfigLoader};
use aws_credential_types::Credentials;
use aws_credential_types::provider::error::CredentialsError;
use aws_credential_types::provider::{self, ProvideCredentials, SharedCredentialsProvider};
use aws_sdk_sts::error::ProvideErrorMetadata;
use aws_types::SdkConfig;
use aws_types::region::Region;
use tokio::time::Instant;

use crate::{ConnectionOptions, Error};

const ROLE_LIFETIME: Duration = Duration::from_secs(3600);
const WARNING_WINDOW: Duration = Duration::from_secs(15 * 60);

/// The explicit assumed role is shared by all clones of a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    /// Standard SDK credentials retain their normal refresh behavior.
    Standard,
    Assumed {
        expires_at: SystemTime,
        remaining: Duration,
    },
    /// The connection has at most fifteen minutes left and will not renew.
    Expiring {
        expires_at: SystemTime,
        remaining: Duration,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallerIdentity {
    pub account: String,
    pub arn: String,
}

#[derive(Clone, Debug)]
struct RoleLease {
    deadline: Instant,
    expires_at: SystemTime,
}

impl RoleLease {
    fn new(expires_at: SystemTime, now: SystemTime) -> Result<Self, Error> {
        let remaining = expires_at
            .duration_since(now)
            .ok()
            .filter(|duration| !duration.is_zero())
            .ok_or(Error::InvalidRoleCredentials)?
            .min(ROLE_LIFETIME);
        Ok(Self {
            deadline: Instant::now() + remaining,
            expires_at: now + remaining,
        })
    }

    fn status(&self) -> Result<SessionStatus, Error> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            Err(Error::ExpiredRole)
        } else if remaining <= WARNING_WINDOW {
            Ok(SessionStatus::Expiring {
                expires_at: self.expires_at,
                remaining,
            })
        } else {
            Ok(SessionStatus::Assumed {
                expires_at: self.expires_at,
                remaining,
            })
        }
    }
}

/// This provider holds the returned credential snapshot; it has no STS client.
/// A monotonic deadline prevents a system-clock adjustment extending the lease.
#[derive(Clone, Debug)]
struct FixedRoleCredentials {
    credentials: Credentials,
    lease: RoleLease,
}

impl ProvideCredentials for FixedRoleCredentials {
    fn provide_credentials<'a>(&'a self) -> provider::future::ProvideCredentials<'a>
    where
        Self: 'a,
    {
        provider::future::ProvideCredentials::ready(
            self.lease
                .status()
                .map(|_| self.credentials.clone())
                .map_err(|_| CredentialsError::provider_error("assumed-role session expired")),
        )
    }
}

/// Connection configuration and a single, optional assumed-role session.
/// Construction resolves credentials; it does not submit an Athena query.
#[derive(Clone, Debug)]
pub struct Connection {
    options: ConnectionOptions,
    config: SdkConfig,
    lease: Option<RoleLease>,
}

impl Connection {
    /// Use standard AWS profiles, region discovery, SSO and ambient credentials.
    /// Call from a Tokio runtime. Unlike the offline literal probe, this method
    /// can make authentication requests through the configured AWS providers.
    pub async fn connect(options: ConnectionOptions) -> Result<Self, Error> {
        Self::with_loader(options, aws_config::defaults(BehaviorVersion::latest())).await
    }

    /// Reuse an already-resolved AWS SDK configuration, including its credential
    /// provider and client overrides. Resolve the profile in that configuration;
    /// `options.profile` must be unset. An explicit region still overrides it.
    /// The optional extra role retains the same fixed lifetime as `connect`.
    pub async fn connect_with_config(
        options: ConnectionOptions,
        mut config: SdkConfig,
    ) -> Result<Self, Error> {
        if options.profile.is_some() {
            return Err(Error::InvalidSetting(
                "resolve the profile before passing an SDK configuration",
            ));
        }
        if let Some(region) = &options.region {
            config = config
                .to_builder()
                .region(Region::new(region.clone()))
                .build();
        }
        Self::from_config(options, config).await
    }

    async fn with_loader(options: ConnectionOptions, loader: ConfigLoader) -> Result<Self, Error> {
        let config = load_config(&options, loader).await?;
        Self::from_config(options, config).await
    }

    pub(crate) async fn from_config(
        options: ConnectionOptions,
        mut config: SdkConfig,
    ) -> Result<Self, Error> {
        options.validate()?;
        if config
            .region()
            .is_none_or(|region| region.as_ref().trim().is_empty())
        {
            return Err(Error::MissingRegion);
        }
        config
            .credentials_provider()
            .ok_or(Error::CredentialsUnavailable)?
            .provide_credentials()
            .await
            .map_err(|_| Error::CredentialsUnavailable)?;

        let lease = if let Some(role) = &options.role_arn {
            // Do not use an auto-refreshing AssumeRoleProvider here. Standard
            // source credentials may refresh, but this extra role must not.
            let response = aws_sdk_sts::Client::new(&config)
                .assume_role()
                .role_arn(role)
                .role_session_name("ckdata-connection")
                .duration_seconds(ROLE_LIFETIME.as_secs() as i32)
                .send()
                .await
                .map_err(|error| Error::AssumeRole {
                    code: error
                        .as_service_error()
                        .and_then(|service| service.code())
                        .unwrap_or("request failed")
                        .to_owned(),
                })?;
            let returned = response
                .credentials()
                .ok_or(Error::InvalidRoleCredentials)?;
            if returned.access_key_id().is_empty()
                || returned.secret_access_key().is_empty()
                || returned.session_token().is_empty()
            {
                return Err(Error::InvalidRoleCredentials);
            }
            let expires_at = SystemTime::try_from(*returned.expiration())
                .map_err(|_| Error::InvalidRoleCredentials)?;
            let now = config
                .time_source()
                .map_or_else(SystemTime::now, |clock| clock.now());
            let lease = RoleLease::new(expires_at, now)?;
            let credentials = Credentials::new(
                returned.access_key_id(),
                returned.secret_access_key(),
                Some(returned.session_token().to_owned()),
                Some(lease.expires_at),
                "ckdata-fixed-role",
            );
            config = config
                .to_builder()
                .credentials_provider(SharedCredentialsProvider::new(FixedRoleCredentials {
                    credentials,
                    lease: lease.clone(),
                }))
                // Resolve this local snapshot on every signing attempt, so the
                // SDK cannot bypass its deadline with a cached identity.
                .identity_cache(IdentityCache::no_cache())
                .build();
            Some(lease)
        } else {
            None
        };
        Ok(Self {
            options,
            config,
            lease,
        })
    }

    pub fn options(&self) -> &ConnectionOptions {
        &self.options
    }

    /// Inspect the selected identity without submitting a data query. Credential
    /// values are never returned. This is useful for acceptance-environment fences.
    pub async fn caller_identity(&self) -> Result<CallerIdentity, Error> {
        self.session_status()?;
        let identity = aws_sdk_sts::Client::new(&self.config)
            .get_caller_identity()
            .send()
            .await
            .map_err(|error| Error::Service {
                operation: "GetCallerIdentity",
                code: error
                    .as_service_error()
                    .and_then(|service| service.code())
                    .unwrap_or("request failed")
                    .to_owned(),
            })?;
        let account = identity
            .account()
            .filter(|account| account.len() == 12 && account.bytes().all(|ch| ch.is_ascii_digit()))
            .ok_or(Error::InvalidServiceResponse("GetCallerIdentity"))?;
        let arn = identity
            .arn()
            .filter(|arn| !arn.is_empty())
            .ok_or(Error::InvalidServiceResponse("GetCallerIdentity"))?;
        Ok(CallerIdentity {
            account: account.to_owned(),
            arn: arn.to_owned(),
        })
    }

    pub(crate) fn aws_config(&self) -> &SdkConfig {
        &self.config
    }

    pub fn region(&self) -> &str {
        // A region is required before a Connection can be constructed.
        self.config.region().expect("validated region").as_ref()
    }

    /// Call before each operation; expiration is a typed error, not renewal.
    pub fn session_status(&self) -> Result<SessionStatus, Error> {
        self.lease
            .as_ref()
            .map_or(Ok(SessionStatus::Standard), RoleLease::status)
    }
}

async fn load_config(
    options: &ConnectionOptions,
    mut loader: ConfigLoader,
) -> Result<SdkConfig, Error> {
    options.validate()?;
    if let Some(profile) = &options.profile {
        loader = loader.profile_name(profile);
    }
    if let Some(region) = &options.region {
        loader = loader.region(Region::new(region.clone()));
    }
    Ok(loader.load().await)
}

#[cfg(test)]
mod tests;
