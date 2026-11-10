#' Reuse results until a daily refresh or a source revision changes
#'
#' Freshness is checked when [collect_saved()] is called. A schedule assumes
#' that the source has refreshed by the specified local time. A revision check
#' can instead report an actual completed build, using a source-specific query
#' or metadata API. Neither policy runs queries in the background.
#'
#' @param at Daily refresh time in `HH:MM` format.
#' @param tz An IANA timezone, such as `"America/Los_Angeles"`.
#' @param check A function accepting a DBI connection and returning one nonempty
#'   string identifying the completed source revision. It must not change data.
#' @param every Seconds to reuse a revision check, per source/access context.
#'   Defaults to 300. Use zero to check on every collection.
#' @return A freshness policy for a connection, [lake_tbl()], [collect_saved()],
#'   or the `ckdata.freshness` option.
#' @details
#' Daily schedules follow daylight saving time. A skipped clock time runs at
#' the transition boundary; a repeated time uses its first occurrence.
#'
#' An ordinary function can also be used directly as a freshness policy. It
#' has the same contract as `check`, but is called on every collection.
#' @export
daily_at <- function(at, tz) {
  at <- single_string(at, "at")
  tz <- single_string(tz, "tz")
  if (!grepl("^([01][0-9]|2[0-3]):[0-5][0-9]$", at)) {
    rlang::abort("ckdata: `at` must be a time in HH:MM format")
  }
  if (!tz %in% OlsonNames()) {
    rlang::abort("ckdata: `tz` must be a recognized IANA timezone")
  }
  structure(list(at = at, tz = tz), class = "ckdata_daily")
}

#' @rdname daily_at
#' @export
freshness_check <- function(check, every = 300) {
  if (!is.function(check)) {
    rlang::abort("ckdata: `check` must be a function accepting a connection")
  }
  rlang::check_number_decimal(every, min = 0, max = Inf)
  structure(
    list(
      check = check,
      every = every,
      revisions = cachem::cache_mem(max_age = every, evict = "fifo")
    ),
    class = "ckdata_revision"
  )
}

check_freshness <- function(x) {
  if (
    rlang::is_null(x) ||
      rlang::is_string(x, "always") ||
      is.function(x) ||
      inherits(x, c("ckdata_daily", "ckdata_revision"))
  ) {
    return(invisible(x))
  }
  rlang::abort(
    "ckdata: `freshness` must be NULL, 'always', daily_at(), freshness_check(), or a revision-checking function"
  )
}

default_freshness <- function() {
  policy <- getOption("ckdata.freshness")
  if (!rlang::is_null(policy)) {
    check_freshness(policy)
    return(policy)
  }
  at <- config_env("CKDATA_FRESHNESS_AT")
  tz <- config_env("CKDATA_FRESHNESS_TZ")
  if (rlang::is_null(at) && rlang::is_null(tz)) {
    return("always")
  }
  if (rlang::is_null(at) || rlang::is_null(tz)) {
    rlang::abort("ckdata: set both CKDATA_FRESHNESS_AT and CKDATA_FRESHNESS_TZ")
  }
  daily_at(at, tz)
}

query_freshness <- function(query) {
  policies <- lazy_freshness(query$lazy_query)
  if (!length(policies)) {
    return(NULL)
  }
  first <- policies[[1L]]
  if (!purrr::every(policies, function(x) identical(x, first))) {
    rlang::abort(
      "ckdata: joined tables have different freshness policies; supply `freshness` to collect_saved()"
    )
  }
  first
}

lazy_freshness <- function(x) {
  if (!is.list(x)) {
    return(list())
  }
  if (inherits(x, "lazy_query") && !rlang::is_null(x$ckdata_freshness)) {
    return(x$ckdata_freshness)
  }
  # dbplyr keeps source queries in the lazy operation tree, including the
  # table list of a multi-join. Reading that tree does not inspect SQL text.
  unlist(purrr::map(x, lazy_freshness), recursive = FALSE)
}

#' @exportS3Method dplyr::collapse
collapse.tbl_ckdata <- function(x, ...) {
  policies <- lazy_freshness(x$lazy_query)
  result <- NextMethod()
  # collapse() creates a new lazy base from SQL. Carry the source policies,
  # including conflicts, so a later collection can still override them.
  result$lazy_query$ckdata_freshness <- policies
  if (inherits(x, "tbl_ckdata_lake")) {
    class(result) <- unique(c("tbl_ckdata_lake", class(result)))
  }
  result
}

resolve_freshness <- function(query, con, policy) {
  check_freshness(policy)
  policy %||%
    query_freshness(query) %||%
    (if (inherits(con, "CkConnection")) con@state$freshness else NULL) %||%
    default_freshness()
}

daily_boundary <- function(policy, now) {
  day <- as.Date(format(now, tz = policy$tz, format = "%Y-%m-%d"))
  boundaries <- as.POSIXct(paste(day - c(0, 1), policy$at), tz = "UTC")
  boundaries <- lubridate::force_tz(
    boundaries,
    policy$tz,
    roll_dst = c("boundary", "pre")
  )
  max(boundaries[boundaries <= now])
}

source_revision <- function(policy, con, context) {
  if (is.function(policy)) {
    return(single_string(policy(con), "source revision"))
  }
  key <- digest::digest(context, algo = "sha256")
  revision <- if (policy$every > 0) {
    policy$revisions$get(key, missing = NULL)
  } else {
    NULL
  }
  if (rlang::is_null(revision)) {
    revision <- single_string(policy$check(con), "source revision")
    if (policy$every > 0) policy$revisions$set(key, revision)
  }
  revision
}

saved_is_fresh <- function(saved, policy, revision, now) {
  if (rlang::is_null(saved)) {
    return(FALSE)
  }
  if (inherits(policy, "ckdata_daily")) {
    return(
      saved$info$fetched_at <= as.numeric(now) &&
        saved$info$fetched_at >= as.numeric(daily_boundary(policy, now))
    )
  }
  if (inherits(policy, "ckdata_revision") || is.function(policy)) {
    return(
      !rlang::is_null(revision) && identical(saved$info$revision, revision)
    )
  }
  FALSE
}
