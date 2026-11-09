#' Collect a query with temporary reuse or an indefinite Parquet snapshot
#'
#' Normal saved results are reused according to `freshness`. Snapshots are
#' created once and reused indefinitely, without automatic refresh or cleanup.
#' Files contain the Arrow result before conversion to R, preserving its types.
#'
#' @param query A lazy dbplyr query.
#' @param freshness Override the table/connection policy, then the application
#'   option/environment defaults. See [daily_at()]. `"always"` fetches each
#'   time; NULL inherits. Without a configured policy, results are fetched anew.
#' @param refresh Force a fresh query in normal mode. Invalid for snapshots.
#' @param snapshot FALSE for normal reuse, TRUE for an automatically named
#'   snapshot, or a filename-safe label such as `"baseline"`.
#' @param dir A directory. Normal mode defaults to `ckdata.saved_dir` or a
#'   session temporary directory. Snapshots default to `ckdata.snapshot_dir` or
#'   the current directory. A persistent directory does not disable freshness.
#' @param source Optional named list describing the source and access context.
#'   Required for other DBI drivers, which do not expose a standard complete
#'   source identity. Include database/namespace, principal/role, tenant and any session
#'   settings affecting visibility or results. Without it, other DBI drivers
#'   cannot safely identify a saved result. Never include credentials.
#' @return A tibble with the current query's grouping.
#' @details
#' Identity includes exact rendered SQL, source/access context and column-policy
#' settings. dbplyr embeds query values in SQL; queries are not canonicalized.
#' Labels supplement this identity, so unrelated queries never share a snapshot.
#' Lake column-policy rules apply only to ckdata connections.
#'
#' A failed save returns the fetched data with a warning. A failed query or
#' revision check returns an available saved result with a stale-data warning;
#' without a saved result, a query failure propagates. A failed revision check
#' without saved data warns and fetches without a reusable revision marker.
#' An existing snapshot that cannot be read raises an error and is preserved;
#' repair it or choose another label/directory. An unreadable normal saved
#' result warns and is fetched again.
#'
#' Normal defaults are temporary and normally removed at R exit. Explicit
#' persistent directories and snapshots are not automatically deleted. Saved
#' files can be read offline; they do not enforce source authorization.
#'
#' Writes publish complete Parquet files atomically. Concurrent misses may run
#' duplicate queries. Normal results keep the last successful publication;
#' snapshots retain and return the first publication. No global ordering
#' is added. Ordinary [dplyr::collect()] always uses its normal query path.
#' @export
collect_saved <- function(
  query,
  freshness = NULL,
  refresh = FALSE,
  snapshot = FALSE,
  dir = NULL,
  source = NULL
) {
  rlang::check_bool(refresh)
  label <- snapshot_label(snapshot)
  frozen <- !identical(snapshot, FALSE)
  if (frozen && (refresh || !rlang::is_null(freshness))) {
    rlang::abort(
      "ckdata: a snapshot cannot specify `refresh` or `freshness`; choose another snapshot label or directory"
    )
  }
  con <- dbplyr::remote_con(query)
  policy <- if (!frozen) resolve_freshness(query, con, freshness) else NULL
  column_policy <- NULL
  if (inherits(con, "CkConnection")) {
    query <- apply_column_policy(query)
    column_policy <- list(mode = ckdata_mode(), allow = ckdata_allow())
  }
  context <- saved_source(con, source)
  sql <- as.character(dbplyr::remote_query(query))
  key <- digest::digest(
    list(
      version = 2L,
      sql = sql,
      source = context,
      mode = column_policy$mode,
      allow = column_policy$allow
    ),
    algo = "sha256"
  )
  path <- saved_path(dir, frozen, label, key)
  saved <- read_saved(path, key, frozen)
  if (frozen && !rlang::is_null(saved)) {
    return(saved_frame(saved$table, query))
  }
  revision <- if (!frozen) {
    read_source_revision(
      policy,
      con,
      context,
      if (refresh) NULL else saved,
      path
    )
  } else {
    NULL
  }
  if (inherits(revision, "ckdata_stale")) {
    return(saved_frame(saved$table, query))
  }
  if (
    !refresh && !frozen && saved_is_fresh(saved, policy, revision, saved_now())
  ) {
    if (ckdata_verbose()) {
      rlang::inform(paste0("ckdata: reading saved result from ", path))
    }
    return(saved_frame(saved$table, query))
  }
  fetched_at <- saved_now()
  table <- rlang::try_fetch(
    arrow::as_arrow_table(DBI::dbGetQueryArrow(con, sql)),
    error = function(cnd) {
      if (rlang::is_null(saved)) {
        rlang::cnd_signal(cnd)
      }
      warn_stale(saved, path, cnd)
      NULL
    }
  )
  if (rlang::is_null(table)) {
    return(saved_frame(saved$table, query))
  }
  frame <- saved_frame(table, query)
  winner <- rlang::try_fetch(
    write_saved(table, path, key, frozen, fetched_at, revision),
    error = function(cnd) {
      if (inherits(cnd, "ckdata_snapshot_unreadable")) {
        rlang::cnd_signal(cnd)
      }
      rlang::warn(
        paste0(
          "ckdata: fetched fresh data, but it was not saved to ",
          path,
          ": ",
          conditionMessage(cnd)
        ),
        class = "ckdata_save_failed",
        parent = cnd
      )
      NULL
    }
  )
  if (!rlang::is_null(winner)) {
    frame <- saved_frame(winner, query)
  }
  frame
}

snapshot_label <- function(snapshot) {
  if (rlang::is_bool(snapshot)) {
    return(NULL)
  }
  snapshot <- single_string(snapshot, "snapshot")
  if (!grepl("^[A-Za-z0-9][A-Za-z0-9._-]*$", snapshot)) {
    rlang::abort(
      "ckdata: a snapshot label must start with a letter or digit and contain only letters, digits, '.', '_' or '-'; use `dir` for the directory"
    )
  }
  snapshot
}

saved_path <- function(dir, frozen, label, key) {
  dir <- dir %||%
    if (frozen) {
      getOption("ckdata.snapshot_dir", ".")
    } else {
      getOption("ckdata.saved_dir", file.path(tempdir(), "ckdata", "saved"))
    }
  dir <- single_string(dir, "dir")
  prefix <- if (frozen) {
    paste0("snapshot-", if (!rlang::is_null(label)) paste0(label, "-"))
  } else {
    "result-"
  }
  file.path(dir, paste0(prefix, key, ".parquet"))
}

saved_now <- function() Sys.time()

saved_source <- function(con, source) {
  if (
    !rlang::is_null(source) && !(rlang::is_named2(source) && is.list(source))
  ) {
    rlang::abort(
      "ckdata: `source` must be a named list identifying the source and access context"
    )
  }
  if (!inherits(con, "CkConnection") && rlang::is_null(source)) {
    rlang::abort(
      "ckdata: supply `source` with the database/namespace and access context for this DBI driver; dbGetInfo() alone does not identify its data reliably"
    )
  }
  info <- DBI::dbGetInfo(con)
  fields <- c(
    "account",
    "arn",
    "region",
    "workgroup",
    "schema",
    "dbname",
    "host",
    "port",
    "username"
  )
  context <- info[intersect(fields, names(info))]
  list(driver = class(con), context = context, source = source)
}

read_saved <- function(path, key, frozen) {
  if (!file.exists(path)) {
    return(NULL)
  }
  rlang::try_fetch(
    {
      table <- arrow::read_parquet(path, as_data_frame = FALSE)
      metadata <- table$schema$metadata[["ckdata.saved"]]
      if (rlang::is_null(metadata)) {
        rlang::abort("missing ckdata saved-result metadata")
      }
      info <- jsonlite::fromJSON(metadata)
      if (
        !identical(info$key, key) ||
          !identical(info$snapshot, frozen) ||
          !is.numeric(info$fetched_at) ||
          length(info$fetched_at) != 1L ||
          !is.finite(info$fetched_at)
      ) {
        rlang::abort("saved-result identity or retrieval time does not match")
      }
      list(table = table, info = info)
    },
    error = function(cnd) {
      # Another process may have removed a file between lookup and opening it.
      if (file.exists(path)) {
        if (frozen) {
          rlang::abort(
            paste0(
              "ckdata: existing snapshot could not be read and was not replaced: ",
              path,
              ". Repair it or choose another snapshot label or directory to fetch anew."
            ),
            class = "ckdata_snapshot_unreadable",
            parent = cnd
          )
        }
        rlang::warn(
          paste0(
            "ckdata: could not read saved result; fetching again: ",
            conditionMessage(cnd)
          ),
          class = "ckdata_saved_unreadable",
          parent = cnd
        )
      }
      NULL
    }
  )
}

read_source_revision <- function(policy, con, context, saved, path) {
  if (!inherits(policy, "ckdata_revision") && !is.function(policy)) {
    return(NULL)
  }
  rlang::try_fetch(
    source_revision(policy, con, context),
    error = function(cnd) {
      if (!rlang::is_null(saved)) {
        warn_stale(saved, path, cnd)
        return(structure(list(), class = "ckdata_stale"))
      }
      rlang::warn(
        paste0(
          "ckdata: could not check source freshness; fetching without a revision marker: ",
          conditionMessage(cnd)
        ),
        class = "ckdata_freshness_failed",
        parent = cnd
      )
      NULL
    }
  )
}

warn_stale <- function(saved, path, cnd) {
  age <- max(0, round(as.numeric(saved_now()) - saved$info$fetched_at))
  rlang::warn(
    paste0(
      "ckdata: refresh failed; returning stale saved data (",
      age,
      " seconds old) from ",
      path,
      ": ",
      conditionMessage(cnd)
    ),
    class = "ckdata_saved_stale",
    parent = cnd
  )
}

write_saved <- function(table, path, key, frozen, fetched_at, revision) {
  dir.create(dirname(path), showWarnings = FALSE, recursive = TRUE)
  temporary <- tempfile(
    ".ckdata-",
    tmpdir = dirname(path),
    fileext = ".parquet"
  )
  withr::defer(unlink(temporary))
  metadata <- table$schema$metadata
  metadata[["ckdata.saved"]] <- jsonlite::toJSON(
    list(
      key = key,
      snapshot = frozen,
      fetched_at = as.numeric(fetched_at),
      revision = revision
    ),
    auto_unbox = TRUE,
    null = "null",
    digits = NA
  )
  table <- table$ReplaceSchemaMetadata(metadata)
  arrow::write_parquet(table, temporary)
  if (frozen) {
    # A hard link publishes the complete sibling file without replacing an
    # existing snapshot. The filesystem arbitrates concurrent writers.
    return(rlang::try_fetch(
      {
        fs::link_create(temporary, path, symbolic = FALSE)
        NULL
      },
      error = function(cnd) {
        winner <- read_saved(path, key, frozen)
        if (rlang::is_null(winner)) {
          rlang::cnd_signal(cnd)
        }
        winner$table
      }
    ))
  }
  # libuv replaces the destination without first deleting a usable result.
  fs::file_move(temporary, path)
  invisible(NULL)
}

saved_frame <- function(table, query) {
  frame <- tibble::as_tibble(table_frame(table))
  dplyr::grouped_df(frame, intersect(dplyr::group_vars(query), names(frame)))
}
