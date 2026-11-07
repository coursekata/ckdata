#' Connection settings
#'
#' Credentials and region use standard AWS profiles, SSO and environment
#' configuration. Set `AWS_PROFILE` and `AWS_REGION` as appropriate, or pass
#' `profile` and `region` to `DBI::dbConnect()`. When an interactive connection
#' needs AWS SSO sign-in, ckdata runs AWS CLI v2 to open the browser, then
#' retries the connection once. Profiles chained through `source_profile`
#' use their SSO source profile. Existing credentials use the normal SDK path.
#'
#' Set `login = FALSE` in `DBI::dbConnect()` or option `ckdata.login = FALSE`
#' to fail immediately without sign-in. The connection argument overrides the
#' option; otherwise the default is [rlang::is_interactive()]. Scripts and jobs
#' therefore default to fail-fast behavior. Explicit TRUE permits sign-in.
#' Cancellation, failed login and a failed retry propagate without a login loop.
#' No browser login is started for permission denials, other authentication
#' methods, or expiry of an existing fixed-role connection.
#'
#' `CK_DATALAKE_WORKGROUP` (or option `ckdata.workgroup`) selects the Athena
#' workgroup. Its configuration supplies the result location and encryption;
#' a separate staging-directory setting is unnecessary. `CK_DATALAKE_SCHEMA`
#' and option `ckdata.schema` select the default database.
#'
#' `CK_DATALAKE_ROLE_ARN` optionally names an additional role to assume once.
#' It expires after at most one hour without renewal. Reconnect explicitly
#' after expiry: call [lake_disconnect()], then rebuild saved lazy queries from
#' a fresh [lake_tbl()]. Saved queries keep their original connection. For an
#' explicit DBI connection, close and recreate it and rebuild its lazy queries.
#' Managed helpers do not silently open another role session.
#'
#' For compatibility, managed helpers also accept `CK_DATALAKE_INTERNAL_PROFILE`
#' and `CK_DATALAKE_REGION`. Explicit DBI connections use the standard AWS
#' chain unless their corresponding arguments are supplied.
#'
#' Saved-result freshness can be set through `CKDATA_FRESHNESS_AT=07:00` and
#' `CKDATA_FRESHNESS_TZ=America/Los_Angeles`, or option `ckdata.freshness`.
#' Explicit `freshness` arguments to [collect_saved()], [lake_tbl()] and
#' `DBI::dbConnect(ckdata(), ...)` override those defaults, in that order.
#' See [daily_at()] for schedules and completed-build revision checks. Normal
#' [dplyr::collect()] is unaffected by saved-result configuration.
#' @name ckdata_settings
NULL
