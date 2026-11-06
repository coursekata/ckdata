#' Connection settings
#'
#' Credentials and region use standard AWS profiles, SSO and environment
#' configuration. Set `AWS_PROFILE` and `AWS_REGION` as appropriate, or pass
#' `profile` and `region` to `DBI::dbConnect()`. ckdata does not start an
#' interactive AWS sign-in itself.
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
#' @name ckdata_settings
NULL
