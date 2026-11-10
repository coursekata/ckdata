#' Connection settings
#'
#' Connection arguments take precedence over configuration. `ckdata.workgroup`
#' and `ckdata.schema` options take precedence over their environment settings.
#' The canonical environment names and compatibility aliases are:
#'
#' | Canonical | Legacy R package alias |
#' | --- | --- |
#' | `CKDATA_PROFILE` | `CK_DATALAKE_INTERNAL_PROFILE` |
#' | `CKDATA_REGION` | `CK_DATALAKE_REGION` |
#' | `CKDATA_WORKGROUP` | `CK_DATALAKE_WORKGROUP` |
#' | `CKDATA_SCHEMA` | `CK_DATALAKE_SCHEMA` |
#' | `CKDATA_ROLE_ARN` | `CK_DATALAKE_ROLE_ARN` |
#' | `CKDATA_OUTPUT_LOCATION` | `CK_DATALAKE_S3_STAGING_DIR` |
#'
#' A nonempty canonical variable takes precedence over its legacy alias.
#' Empty and absent variables both count as unset on every platform. To bypass
#' an inherited optional setting, pass an explicit NULL connection argument.
#' Aliases are resolved without changing the process's AWS environment.
#'
#' Both managed helpers and explicit DBI connections use this configuration.
#' Explicit NULL for `profile`, `region`, `role_arn` or `output_location` bypasses
#' that setting. Without package-specific profile or region settings, the normal
#' AWS SDK chain applies, including `AWS_PROFILE`, `AWS_REGION`, shared profiles
#' and workload credentials. No account, role, region or workgroup is built in.
#' The schema defaults to `"default"`; configure it explicitly for CourseKata.
#'
#' The workgroup supplies the result location and encryption settings by default.
#' `CKDATA_OUTPUT_LOCATION` can supply or override the S3 prefix when the workgroup
#' permits client settings. Enforced workgroup settings always take precedence.
#' The legacy staging-directory variable therefore remains functional without
#' changing the workgroup or creating AWS resources.
#'
#' A named `profile` argument selects that profile's authentication provider,
#' ignoring SSO environment settings and unrelated ambient access keys. Without
#' an explicit profile, `CKDATA_PROFILE` (or its legacy alias) selects a profile
#' unless direct SSO settings take precedence. Without ckdata authentication
#' settings, the standard AWS credential chain applies, including `AWS_PROFILE`.
#'
#' Direct SSO uses `sso_start_url`, `sso_region`, `sso_account_id` and
#' `sso_role_name`, or the corresponding `CKDATA_SSO_*` environment variables.
#' Individual arguments override their environment settings. Missing fields can
#' come from one selected plain SSO profile; role chains are never inherited by
#' direct SSO. Individual SSO overrides of an explicitly selected chained profile
#' are rejected. `CKDATA_SSO_SESSION` optionally names a shared login session;
#' `CKDATA_SSO_REGISTRATION_SCOPES` defaults to `sso:account:access`.
#'
#' `CKDATA_ROLE_ARN` optionally requires an additional role. AWS providers renew
#' temporary credentials while their source permits; there is no fixed connection
#' lifetime. A profile already targeting the exact required role is reused.
#' Other sources must assume it successfully; failure never permits data access
#' with the source identity. No extra IAM lookup permission is needed.
#'
#' Interactive R connections offer AWS SSO browser sign-in through AWS CLI v2
#' when silent renewal cannot restore the resolved SSO source. A running request
#' stays in its original native job during sign-in; SQL is not resubmitted.
#' Set `login = FALSE` in `DBI::dbConnect()` or `options(ckdata.login = FALSE)`
#' to fail instead. The argument overrides the option; otherwise the default is
#' [rlang::is_interactive()]. Scripts and jobs default to fail-fast. This controls
#' ckdata's login process, not an external profile `credential_process` helper.
#' Each top-level connection, query or fetch permits one browser login and one
#' credential retry. Failed login, cancellation or a failed retry stops the
#' operation. Permission denials never trigger browser login or profile hunting.
#' AWS owns token caching; direct SSO uses a private subprocess-only config file
#' that is removed after the login process exits. The R process's AWS environment
#' and the user's AWS configuration are not modified.
#'
#' Saved-result freshness uses `CKDATA_FRESHNESS_AT` and `CKDATA_FRESHNESS_TZ`, or
#' option `ckdata.freshness`. No schedule or timezone is built in. Collection,
#' table and connection policies take precedence, in that order. See [daily_at()]
#' for schedules and completed-build revision checks. Normal [dplyr::collect()]
#' is unaffected by saved-result configuration.
#' @name ckdata_settings
NULL

# Resolve compatibility at the R configuration boundary, without changing the
# process's AWS environment. Blank and absent settings use the same fallback.
config_env <- function(name) {
  aliases <- c(
    CKDATA_PROFILE = "CK_DATALAKE_INTERNAL_PROFILE",
    CKDATA_REGION = "CK_DATALAKE_REGION",
    CKDATA_WORKGROUP = "CK_DATALAKE_WORKGROUP",
    CKDATA_SCHEMA = "CK_DATALAKE_SCHEMA",
    CKDATA_ROLE_ARN = "CK_DATALAKE_ROLE_ARN",
    CKDATA_OUTPUT_LOCATION = "CK_DATALAKE_S3_STAGING_DIR"
  )
  value <- Sys.getenv(name, unset = "")
  legacy <- unname(aliases[name])
  if (!nzchar(value) && !is.na(legacy)) {
    value <- Sys.getenv(legacy, unset = "")
  }
  if (nzchar(value)) value else NULL
}
