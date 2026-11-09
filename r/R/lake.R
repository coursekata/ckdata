lake_state <- rlang::new_environment()
lake_state$tables <- tibble::tibble(
  path = character(),
  table = character(),
  column = character()
)

#' A lazy table in Amazon Athena
#'
#' Named-table discovery uses catalog metadata without reading rows. Building
#' a pipeline or calling `show_query()` does not execute it. As with other
#' dbplyr tables, printing previews rows and `collect()` retrieves results.
#'
#' Results have no global order across Parquet files. To order a downloaded
#' result, use `collect()` followed by `arrange()`.
#'
#' A saved lazy query keeps its original connection. After [lake_disconnect()],
#' rebuild it from a fresh `lake_tbl()` before collecting results.
#' @param table A table name.
#' @param schema Database name; defaults to [ckdata_schema()].
#' @param freshness Optional [collect_saved()] policy for queries derived from
#'   this table. Overrides connection and application defaults without changing
#'   other tables. Conflicting policies in a join require a collection override.
#' @return A lazy dbplyr table using the managed session connection.
#' @export
lake_tbl <- function(table, schema = ckdata_schema(), freshness = NULL) {
  check_freshness(freshness)
  table <- single_string(table, "table")
  schema <- single_string(schema, "schema")
  tbl <- dplyr::tbl(lake_con(), DBI::Id(schema = schema, table = table))
  tbl$lazy_query$ckdata_freshness <- if (!rlang::is_null(freshness)) {
    list(freshness)
  }
  class(tbl) <- c("tbl_ckdata_lake", class(tbl))
  register_lake_table(tbl)
  tbl
}

#' @exportS3Method dplyr::collect
collect.tbl_ckdata_lake <- function(x, ...) {
  x <- apply_column_policy(x)
  NextMethod()
}

#' Execute SQL through the shared query core
#'
#' SQL queries do not receive the dbplyr column-policy heuristics. Empty
#' results retain their schema. The Arrow output retains the original Arrow
#' types; tibble output uses the documented R conversion in [ckdata_types].
#' The Parquet UNLOAD path requires named output columns and known types.
#' Alias expressions (`SELECT 1 AS example`) and cast standalone NULL outputs
#' (`SELECT CAST(NULL AS VARCHAR) AS example`). SQL is not rewritten to add them.
#' @param sql SQL query text.
#' @param as Either `"tibble"` or `"arrow"`.
#' @param con A ckdata DBI connection; defaults to the managed connection.
#' @return A tibble, or an Arrow Table when `as = "arrow"`.
#' @export
lake_query <- function(sql, as = c("tibble", "arrow"), con = lake_con()) {
  as <- rlang::arg_match(as)
  if (as == "arrow") {
    return(arrow::as_arrow_table(DBI::dbGetQueryArrow(con, sql)))
  }
  tibble::as_tibble(DBI::dbGetQuery(con, sql), .name_repair = "minimal")
}

register_lake_table <- function(tbl) {
  entry <- lake_table_columns(tbl)
  lake_state$tables <- lake_state$tables |>
    dplyr::anti_join(entry, by = "path") |>
    dplyr::bind_rows(entry)
}

lake_table_columns <- function(tbl) {
  path <- dbplyr::remote_table(tbl)
  table <- dbplyr::remote_name(tbl)
  tibble::tibble(
    path = as.character(path),
    table = table,
    column = colnames(tbl)
  )
}

lake_con <- function() {
  # Credential renewal retains this connection and its source configuration.
  # Only initial use or explicit disconnection selects new settings.
  if (
    rlang::is_null(lake_state$con) || !rlang::is_true(lake_state$con@state$open)
  ) {
    lake_state$con <- DBI::dbConnect(ckdata())
    lake_state$tables <- lake_state$tables[0, ]
  }
  lake_state$con
}

#' Close the managed lake session
#'
#' The next call to [lake_tbl()] or [lake_query()] opens a new connection using
#' the current settings. Saved lazy queries keep their original connection;
#' rebuild them from a fresh `lake_tbl()` before collecting results.
#' Use this explicitly before changing
#' connection settings. Calling it before a session exists does not connect.
#' @return Invisibly, `TRUE`.
#' @export
lake_disconnect <- function() {
  if (
    !rlang::is_null(lake_state$con) && rlang::is_true(lake_state$con@state$open)
  ) {
    DBI::dbDisconnect(lake_state$con)
  }
  invisible(TRUE)
}
