//! Nonsecret SSO recovery metadata. Login UI and processes belong to the host.
use std::collections::HashSet;
use std::error::Error as StdError;

use aws_config::profile::ProfileSet;
use aws_credential_types::provider::error::{CredentialsError, TokenError};
use aws_runtime::env_config::file::EnvConfigFiles;
use aws_sdk_sso::operation::get_role_credentials::GetRoleCredentialsError;
use aws_types::os_shim_internal::{Env, Fs};

use crate::Error;

/// The SSO source profile and optional named session to pass to AWS's login tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SsoLoginTarget {
    pub profile: String,
    pub session: Option<String>,
}

pub(crate) async fn login_target(profile: Option<&str>) -> Option<SsoLoginTarget> {
    let profiles = aws_config::profile::load(
        &Fs::real(),
        &Env::real(),
        &EnvConfigFiles::default(),
        profile.map(|name| name.to_owned().into()),
    )
    .await
    .ok()?;
    profile_login_target(&profiles)
}

fn profile_login_target(profiles: &ProfileSet) -> Option<SsoLoginTarget> {
    let mut name = profiles.selected_profile();
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(name) {
            return None;
        }
        let profile = profiles.get_profile(name)?;
        // The SDK prioritizes complete static credentials after a role hop,
        // but at the selected base profile SSO precedes static/process fields.
        if visited.len() > 1
            && profile.get("aws_access_key_id").is_some()
            && profile.get("aws_secret_access_key").is_some()
        {
            return None;
        }
        if profile.get("credential_source").is_some()
            || profile.get("web_identity_token_file").is_some()
        {
            return None;
        }
        // source_profile is meaningful only for a role. A self-reference uses
        // this profile's base provider rather than forming another chain hop.
        if profile.get("role_arn").is_some() {
            let source = profile.get("source_profile")?;
            if source != name {
                name = source;
                continue;
            }
        }
        profile.get("sso_account_id")?;
        profile.get("sso_role_name")?;
        let session = profile.get("sso_session");
        if let Some(session) = session {
            let settings = profiles.sso_session(session)?;
            settings.get("sso_start_url")?;
            settings.get("sso_region")?;
        } else {
            profile.get("sso_start_url")?;
            profile.get("sso_region")?;
        }
        return Some(SsoLoginTarget {
            profile: name.to_owned(),
            session: session.map(str::to_owned),
        });
    }
}

pub(crate) fn credential_error(error: CredentialsError, target: Option<SsoLoginTarget>) -> Error {
    // Inspect typed SDK causes, never authentication error text or token files.
    let mut token_error = false;
    let mut missing_file = false;
    let mut unauthorized = false;
    let mut source: Option<&(dyn StdError + 'static)> = Some(&error);
    while let Some(cause) = source {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            match io.kind() {
                std::io::ErrorKind::NotFound => missing_file = true,
                _ => return Error::CredentialsUnavailable,
            }
        }
        token_error |= cause.is::<TokenError>();
        if let Some(sdk) =
            cause.downcast_ref::<aws_sdk_sso::error::SdkError<GetRoleCredentialsError>>()
        {
            unauthorized |= sdk
                .as_service_error()
                .is_some_and(GetRoleCredentialsError::is_unauthorized_exception);
        }
        source = cause.source().or_else(|| {
            // aws-config 1.13's private SSO token wrapper implements cause(),
            // not source(). Its cache-error child exposes the typed I/O cause.
            // Keep permission/configuration faults out of browser recovery.
            #[allow(deprecated)]
            cause.cause().and_then(StdError::source)
        });
    }
    if (token_error || missing_file || unauthorized)
        && let Some(target) = target
    {
        Error::SsoLoginRequired { target }
    } else {
        Error::CredentialsUnavailable
    }
}

#[cfg(test)]
mod tests;
