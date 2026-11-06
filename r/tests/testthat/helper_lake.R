# CourseKata's existing public-behavior fixtures use invented SQLite rows.
local_lake_session <- function(con, env = rlang::caller_env()) {
  testthat::local_mocked_bindings(lake_con = function() con, .package = "ckdata", .env = env)
  rlang::local_bindings(tables = lake_state$tables[0, ], .env = lake_state, .frame = env)
}

# A throwaway in-memory SQLite database holding `df` as table `name`, closed when the calling test
# ends.
local_memory_con <- function(df, name, env = rlang::caller_env()) {
  testthat::skip_if_not_installed("RSQLite")
  con <- DBI::dbConnect(RSQLite::SQLite(), ":memory:")
  withr::defer(DBI::dbDisconnect(con), envir = env)
  DBI::dbWriteTable(con, name, df)
  con
}

# A plain lazy table (not a lake table) over an in-memory SQLite copy of `df`.
local_memory_tbl <- function(df, name = "t", env = rlang::caller_env()) {
  dplyr::tbl(local_memory_con(df, name, env), name)
}

# `df` as lake table `name`, read from an in-memory SQLite copy that stands in for the lake.
local_memory_lake_tbl <- function(df, name = "t", env = rlang::caller_env()) {
  local_lake_session(local_memory_con(df, name, env), env)
  lake_tbl(name, schema = "main")
}

# int_classes as a lake table over an in-memory SQLite copy. Every column of an intermediate table
# is flagged, so the edges can be tested without depending on the shipped deprecation data.
local_classes_lake_tbl <- function(env = rlang::caller_env()) {
  local_memory_lake_tbl(data.frame(class_id = 1:2, is_test = FALSE), "int_classes", env = env)
}

# A ckdata query with real SQLite execution for cache and column-policy tests.
local_saved_tbl <- function(df, name = "t", env = rlang::caller_env()) {
  fixture <- local_native_connection(env)
  source <- local_memory_con(df, name, env)
  local_lake_session(fixture$con, env)
  testthat::local_mocked_bindings(
    dbGetQueryArrow = function(conn, statement, ...) {
      fixture$calls$queries <- c(fixture$calls$queries, statement)
      table <- arrow::Table$create(DBI::dbGetQuery(source, statement, ...))
      nanoarrow::as_nanoarrow_array_stream(table)
    },
    .package = "DBI", .env = env
  )
  query <- dplyr::tbl(fixture$con, DBI::Id(schema = "main", table = name), vars = names(df))
  register_lake_table(query)
  list(query = query, source = source, calls = fixture$calls)
}

local_saved_policy <- function(env = rlang::caller_env()) {
  withr::local_options(list(ckdata.freshness = daily_at("00:00", "UTC")), .local_envir = env)
}

# A plain dbplyr table over dbplyr's simulated connection: it renders SQL but never runs it, so the
# column check, which reads only a query's SQL and columns, runs offline.
simulated_tbl <- function(table, columns, schema = "ck_datalake_prd_marts_app") {
  dplyr::tbl(dbplyr::simulate_dbi(), dbplyr::in_schema(schema, table), vars = columns)
}

# Registry rows for these lake tables, as lake_tbl() records them.
registry_of <- function(...) {
  dplyr::bind_rows(purrr::map(list(...), lake_table_columns))
}

# Real rows from the shipped column table, one per kind of replacement: a column on the same
# table, a column on another table, another table with no single column, and none.
deprecation_fixture <- tibble::tribble(
  ~table,           ~column,                 ~new_table,         ~new_column,       ~removed_on,
  "mart_responses", "branch",                "mart_enrollments", "variation_index", "2026-12-01",
  "mart_responses", "lms_id",                NA,                 NA,                "2026-12-01",
  "mart_responses", "lrn_question_position", "mart_questions",   NA,                "2026-12-01",
  "mart_responses", "lrn_type",              "mart_responses",   "question_type",   "2026-12-01"
)
