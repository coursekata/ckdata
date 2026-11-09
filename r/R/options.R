#' Options that control ckdata
#'
#' Set these with [options()], for one session or in your `.Rprofile`.
#'
#' - `ckdata.mode`: what ckdata does when a query reads a column that is deprecated or may change
#'   without notice. One of `"explore"` (the default), `"audit"` or `"publish"`; see Modes below.
#' - `ckdata.allow`: flagged columns to accept anyway, as a named list of `table = column names`,
#'   for example `list(mart_responses = c("lrn_type", "item_id"))`. An allowed column is never
#'   reported or removed, in any mode.
#' - `ckdata.verbose`: `TRUE` reports when [collect_saved()] reuses a saved
#'   result. Defaults to `FALSE`. Core failures remain
#'   visible in either setting.
#' - `ckdata.timeout`: query timeout in seconds, including result download and
#'   empty-schema recovery. Defaults to 1800; a cancellation stop request can
#'   require additional time as documented by the shared core.
#' - `ckdata.login`: allow browser-based AWS SSO sign-in on a connection and
#'   its later operations. Captured when the connection is opened. Defaults to [rlang::is_interactive()]. FALSE fails immediately;
#'   an explicit `DBI::dbConnect(..., login = ...)` overrides this option.
#'   Browser sign-in uses AWS CLI v2 and retries credential acquisition once
#'   per top-level operation, preserving the existing connection and query.
#' - `ckdata.freshness`: default [collect_saved()] policy; see [daily_at()].
#'   Explicit collection, table and connection policies take precedence, in
#'   that order. The option overrides `CKDATA_FRESHNESS_AT` and
#'   `CKDATA_FRESHNESS_TZ`, which together specify a daily schedule. Without a
#'   policy, collection fetches anew. Application defaults are read at collection
#'   time. A schedule assumes the source has refreshed by that time.
#' - `ckdata.saved_dir`: normal saved-result directory. Defaults to a ckdata
#'   subdirectory of R's session temporary directory, normally removed at exit.
#'   An explicit persistent directory keeps files across sessions; freshness
#'   still applies, and ckdata does not automatically delete those files.
#' - `ckdata.snapshot_dir`: indefinite snapshot directory. Defaults to the
#'   current directory. Snapshots never automatically refresh or expire.
#' - `ckdata.schema`: the database [lake_tbl()] opens a table from when the call names none; see
#'   [ckdata_schema()].
#'
#' @section Modes:
#' Two kinds of column are flagged:
#'
#' - **Deprecated** columns, which the lake keeps under an old name until a set removal date. The
#'   report names each one's replacement.
#' - Every column of a **staging** (`stg_`) or **intermediate** (`int_`) table, which can change
#'   without notice, unless a published table in the same query has it too, as a join key does.
#'
#' ckdata checks a query when [dplyr::collect()] or [collect_saved()] runs it. The check reads only
#' the query's SQL and columns, never the data.
#'
#' - `"explore"`: nothing is checked. Use it while you work things out.
#' - `"audit"`: a warning names the flagged columns the query uses or returns, and nothing else
#'   changes, so one run of a script lists what to fix and the script still finishes.
#' - `"publish"`: the same warning, and flagged columns the query returns are removed from it before
#'   it runs, so code that reads them fails now rather than on the removal date. A query that would
#'   return nothing else fails instead. Use it for the final run of a document.
#'
#' The modes are a helper for the common cases, not a guarantee. Because the check reads only the
#' SQL, a flagged column that is renamed, or that a join returns as `.x` or `.y`, is reported but
#' not removed; a query whose first table did not come from [lake_tbl()] is checked only by
#' [collect_saved()] on a ckdata connection; and now and then a warning names a
#' column the query did not need. Other DBI drivers do not use lake column rules.
#' @seealso [ckdata_settings]
#' @name ckdata_options
NULL

ckdata_verbose <- function() {
  rlang::is_true(getOption("ckdata.verbose"))
}

#' The database lake_tbl() reads from by default
#'
#' [lake_tbl()] opens a table from this database when the call names none. It is the
#' `ckdata.schema` option when that is set, then `CKDATA_SCHEMA` (or its legacy alias `CK_DATALAKE_SCHEMA`), and
#' otherwise `"default"`. An empty variable counts as unset. No organization-specific
#' database or AWS account is selected by the package.
#' The option and the variable are read on every call, so a change applies to the next [lake_tbl()]
#' call.
#'
#' Set the option in a script or `.Rprofile`. Set the variable where the environment rather than the
#' script decides: an environment that may read only the `research` views, such as a research
#' container, sets `CKDATA_SCHEMA=research`, and its scripts name no database.
#'
#' @return The database's name.
#' @seealso [ckdata_options], [ckdata_settings]
#' @export
#' @examples
#' ckdata_schema()
#'
#' old <- options(ckdata.schema = "research")
#' ckdata_schema()
#' options(old)
ckdata_schema <- function() {
  getOption("ckdata.schema") %||%
    config_env("CKDATA_SCHEMA") %||%
    "default"
}

ckdata_mode <- function() {
  rlang::arg_match0(
    getOption("ckdata.mode", "explore"),
    c("explore", "audit", "publish"),
    arg_nm = "ckdata.mode",
    error_call = NULL
  )
}

ckdata_allow <- function() {
  allow <- getOption("ckdata.allow", list())
  valid <- rlang::is_list(allow) &&
    rlang::is_named2(allow) &&
    purrr::every(allow, rlang::is_character)
  if (!valid) {
    cli::cli_abort(
      c(
        "{.code ckdata.allow} must be a named list of column names.",
        i = 'For example: {.code options(ckdata.allow = list(mart_responses = "lrn_type"))}'
      ),
      class = "ckdata_bad_option",
      call = NULL
    )
  }
  allow
}
