#' Collect a query with an explicit local Parquet cache
#'
#' Cache entries depend on SQL and the authenticated connection context,
#' including account, principal, region, workgroup and schema. A cache does not
#' detect changes to source data: use `refresh = TRUE` to query again.
#'
#' @param query A lazy dbplyr query using a ckdata connection.
#' @param refresh Ignore the cached result and query again.
#' @param dir Cache directory; defaults to `ckdata.cache_dir` or `.ckdata-cache`.
#' @return A tibble.
#' @export
collect_cached <- function(query, refresh = FALSE,
                           dir = getOption("ckdata.cache_dir", ".ckdata-cache")) {
  rlang::check_bool(refresh)
  dir <- single_string(dir, "dir")
  con <- dbplyr::remote_con(query)
  if (!inherits(con, "CkConnection")) {
    rlang::abort("ckdata: `query` must use a ckdata connection")
  }
  query <- apply_column_policy(query)
  info <- DBI::dbGetInfo(con)
  context <- info[intersect(names(info), c("account", "arn", "region", "workgroup", "schema", "dbname"))]
  key <- digest::digest(list(
    version = 1L,
    sql = as.character(dbplyr::remote_query(query)), context = context,
    mode = ckdata_mode(), allow = ckdata_allow()
  ))
  path <- file.path(dir, paste0(key, ".parquet"))
  if (!refresh && file.exists(path)) {
    if (ckdata_verbose()) rlang::inform(paste0("ckdata: reading cached result from ", path))
    return(cache_frame(arrow::read_parquet(path, as_data_frame = FALSE), query))
  }
  table <- arrow::as_arrow_table(DBI::dbGetQueryArrow(con, as.character(dbplyr::remote_query(query))))
  df <- cache_frame(table, query)
  dir.create(dir, showWarnings = FALSE, recursive = TRUE)
  temporary <- tempfile("ckdata-", tmpdir = dir, fileext = ".parquet")
  withr::defer(unlink(temporary))
  arrow::write_parquet(table, temporary)
  # libuv replaces the destination without first deleting a usable cache entry.
  fs::file_move(temporary, path)
  df
}

cache_frame <- function(table, query) {
  frame <- tibble::as_tibble(table_frame(table))
  dplyr::grouped_df(frame, intersect(dplyr::group_vars(query), names(frame)))
}
