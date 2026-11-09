use crate::Error;

/// AWS discovery uses the standard SDK chain. The query destination is explicit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionOptions {
    pub workgroup: String,
    pub schema: String,
    pub profile: Option<String>,
    pub region: Option<String>,
    /// Direct SSO settings, or overrides of an explicitly selected plain SSO profile.
    pub sso: Option<crate::auth::SsoOptions>,
    /// A required additional role, obtained and renewed by the AWS SDK.
    pub role_arn: Option<String>,
    /// Client result prefix. Enforced workgroup configuration takes precedence.
    pub output_location: Option<String>,
}

impl ConnectionOptions {
    pub fn new(workgroup: impl Into<String>, schema: impl Into<String>) -> Self {
        Self {
            workgroup: workgroup.into(),
            schema: schema.into(),
            profile: None,
            region: None,
            sso: None,
            role_arn: None,
            output_location: None,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self
            .output_location
            .as_deref()
            .is_some_and(|value| crate::location::S3Location::prefix(value).is_none())
        {
            return Err(Error::InvalidSetting(
                "output location must be an S3 prefix",
            ));
        }
        if self.workgroup.is_empty()
            || self.workgroup.len() > 128
            || !self
                .workgroup
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        {
            return Err(Error::InvalidSetting("workgroup"));
        }
        for (name, value) in [
            ("schema", Some(self.schema.as_str())),
            ("profile", self.profile.as_deref()),
            ("region", self.region.as_deref()),
        ] {
            if value.is_some_and(|v| v.trim().is_empty() || v.chars().any(char::is_control)) {
                return Err(Error::InvalidSetting(name));
            }
        }
        if let Some(role) = &self.role_arn {
            let parts: Vec<_> = role.splitn(6, ':').collect();
            let valid = parts.len() == 6
                && parts[0] == "arn"
                && !parts[1].is_empty()
                && parts[1]
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c == b'-')
                && parts[2] == "iam"
                && parts[3].is_empty()
                && parts[4].len() == 12
                && parts[4].bytes().all(|c| c.is_ascii_digit())
                && parts[5].starts_with("role/")
                && parts[5].len() > 5
                && !parts[5].ends_with('/')
                && parts[5][5..]
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"+=,.@_/-".contains(&c));
            if !valid {
                return Err(Error::InvalidSetting("role_arn"));
            }
        }
        Ok(())
    }
}
