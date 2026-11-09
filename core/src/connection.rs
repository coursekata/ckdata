use aws_config::BehaviorVersion;
use aws_credential_types::provider::{ProvideCredentials, SharedCredentialsProvider};
use aws_sdk_sts::error::ProvideErrorMetadata;
use aws_types::SdkConfig;
use aws_types::region::Region;

use crate::{ConnectionOptions, Error};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallerIdentity {
    pub account: String,
    pub arn: String,
}

/// Connection configuration and AWS-managed renewable credential providers.
/// Construction resolves credentials; it does not submit an Athena query.
#[derive(Clone, Debug)]
pub struct Connection {
    options: ConnectionOptions,
    config: SdkConfig,
    login: Option<crate::SsoLoginTarget>,
}

impl Connection {
    /// Use standard AWS profiles, region discovery, SSO and ambient credentials.
    /// Call from a Tokio runtime. This can make authentication requests through
    /// the configured AWS providers; it never opens a browser or runs login.
    pub async fn connect(options: ConnectionOptions) -> Result<Self, Error> {
        let auth =
            crate::auth::configure(&options, aws_config::defaults(BehaviorVersion::latest()))
                .await?;
        Self::from_auth(options, auth).await
    }

    pub(crate) async fn from_auth(
        options: ConnectionOptions,
        auth: crate::auth::ConfiguredAuth,
    ) -> Result<Self, Error> {
        let bound = auth.role_binding.as_deref() == options.role_arn.as_deref()
            && options.role_arn.is_some();
        Self::build(options, auth.config, auth.target, bound).await
    }

    /// Reuse an already-resolved AWS SDK configuration, including its credential
    /// provider and client overrides. Resolve the profile in that configuration;
    /// `options.profile` must be unset. An explicit region still overrides it.
    /// An optional required role uses AWS-managed renewal.
    pub async fn connect_with_config(
        options: ConnectionOptions,
        mut config: SdkConfig,
    ) -> Result<Self, Error> {
        if options.profile.is_some() || options.sso.is_some() {
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

    pub(crate) async fn from_config(
        options: ConnectionOptions,
        config: SdkConfig,
    ) -> Result<Self, Error> {
        Self::build(options, config, None, false).await
    }

    async fn build(
        options: ConnectionOptions,
        mut config: SdkConfig,
        login: Option<crate::SsoLoginTarget>,
        role_bound: bool,
    ) -> Result<Self, Error> {
        options.validate()?;
        if config
            .region()
            .is_none_or(|region| region.as_ref().trim().is_empty())
        {
            return Err(Error::MissingRegion);
        }
        if crate::SsoLogin::enabled() || config.identity_cache().is_none() {
            // Browser interaction is bounded by the host/query deadline, not
            // the SDK's short default identity-load timeout.
            let cache = aws_config::identity::IdentityCache::lazy();
            let cache = if crate::SsoLogin::enabled() {
                cache.load_timeout(std::time::Duration::from_secs(1800))
            } else {
                cache
            };
            config = config.to_builder().identity_cache(cache.build()).build();
        }
        if let Some(role) = &options.role_arn
            && !role_bound
        {
            let provider = aws_config::sts::AssumeRoleProvider::builder(role)
                .session_name("ckdata-connection")
                .configure(&config)
                .build()
                .await;
            config = config
                .to_builder()
                .credentials_provider(SharedCredentialsProvider::new(provider))
                .build();
        }
        let credentials = config
            .credentials_provider()
            .ok_or(Error::CredentialsUnavailable)?
            .provide_credentials()
            .await
            .map_err(|error| crate::auth::credential_error(error, login.clone()))?;
        let now = config
            .time_source()
            .map_or_else(std::time::SystemTime::now, |clock| clock.now());
        if credentials.expiry().is_some_and(|expiry| expiry <= now) {
            return Err(Error::InvalidRoleCredentials);
        }
        Ok(Self {
            options,
            config,
            login,
        })
    }

    pub fn options(&self) -> &ConnectionOptions {
        &self.options
    }

    /// Select another schema while sharing the same renewable AWS providers.
    pub fn with_schema(&self, schema: &str) -> Result<Self, Error> {
        let mut connection = self.clone();
        connection.options.schema = schema.to_owned();
        connection.options.validate()?;
        Ok(connection)
    }

    /// Inspect the selected identity without submitting a data query. Credential
    /// values are never returned. This is useful for acceptance-environment fences.
    pub async fn caller_identity(&self) -> Result<CallerIdentity, Error> {
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
                message: crate::error::service_message(error.as_service_error()),
            })?;
        let account = identity
            .account()
            .filter(|account| account.len() == 12 && account.bytes().all(|ch| ch.is_ascii_digit()))
            .ok_or(Error::InvalidServiceResponse("GetCallerIdentity"))?;
        let arn = identity
            .arn()
            .filter(|arn| !arn.is_empty())
            .ok_or(Error::InvalidServiceResponse("GetCallerIdentity"))?;
        let identity = CallerIdentity {
            account: account.to_owned(),
            arn: arn.to_owned(),
        };
        if let Some(role) = &self.options.role_arn {
            validate_role_identity(&identity, role)?;
        }
        Ok(identity)
    }

    pub(crate) fn aws_config(&self) -> &SdkConfig {
        &self.config
    }

    pub(crate) fn request_timeout(&self, normal: std::time::Duration) -> std::time::Duration {
        if self.login.is_some() && crate::SsoLogin::enabled() {
            std::time::Duration::from_secs(1800)
        } else {
            normal
        }
    }

    pub fn region(&self) -> &str {
        // A region is required before a Connection can be constructed.
        self.config.region().expect("validated region").as_ref()
    }
}

fn validate_role_identity(identity: &CallerIdentity, role: &str) -> Result<(), Error> {
    // The configured AWS provider proves the full target ARN. STS identity
    // checks its runtime account, partition and role name; it cannot supply an
    // IAM role's path and is never used to infer arbitrary credentials' ARN.
    let parts: Vec<_> = role.splitn(6, ':').collect();
    let name = parts[5].rsplit('/').next().unwrap();
    let prefix = format!("arn:{}:sts::{}:assumed-role/{name}/", parts[1], parts[4]);
    if identity.account != parts[4] || !identity.arn.starts_with(&prefix) {
        return Err(Error::RoleIdentityMismatch {
            expected: role.to_owned(),
            actual: identity.arn.clone(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
