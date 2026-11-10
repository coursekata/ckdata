test_that("saved Athena results reuse, refresh and preserve exact Parquet snapshots", {
  context <- local_live_context()
  con <- context$con
  namespace <- rlang::ns_env("ckdata")
  native_query <- rlang::env_get(namespace, "native_query")
  calls <- 0L
  # Count submissions while retaining the real AWS/Arrow path.
  testthat::local_mocked_bindings(
    native_query = function(...) {
      calls <<- calls + 1L
      native_query(...)
    },
    .package = "ckdata"
  )
  withr::local_options(ckdata.freshness = daily_at("00:00", "UTC"))
  sql <- "SELECT BIGINT '9007199254740993' AS big, BIGINT '-9223372036854775808' AS minimum, CAST('12345678901234567890.123456789' AS DECIMAL(38,9)) AS wide, ARRAY[CAST('1.10' AS DECIMAL(20,2)), CAST(NULL AS DECIMAL(20,2))] AS nested"
  columns <- c("big", "minimum", "wide", "nested")
  query <- dplyr::tbl(con, dbplyr::sql(sql), vars = columns)
  directory <- withr::local_tempdir(pattern = "ckdata-synthetic-saved-")
  first <- collect_saved(query, dir = directory)
  testthat::expect_identical(collect_saved(query, dir = directory), first)
  testthat::expect_identical(calls, 1L)
  testthat::expect_identical(
    collect_saved(query, refresh = TRUE, dir = directory),
    first
  )
  testthat::expect_identical(calls, 2L)
  snapshot <- collect_saved(query, snapshot = "baseline", dir = directory)
  testthat::expect_identical(
    collect_saved(query, snapshot = "baseline", dir = directory),
    snapshot
  )
  testthat::expect_identical(calls, 3L)
  file <- list.files(
    directory,
    pattern = "^snapshot-baseline-",
    full.names = TRUE
  )
  parquet <- arrow::read_parquet(file, as_data_frame = FALSE)
  expected <- list(
    big = arrow::Array$create("9007199254740993")$cast(arrow::int64()),
    minimum = arrow::Array$create("-9223372036854775808")$cast(arrow::int64()),
    wide = arrow::Array$create(
      "12345678901234567890.123456789"
    )$cast(arrow::decimal128(38, 9))
  )
  purrr::iwalk(expected, function(value, name) {
    testthat::expect_true(parquet[[name]]$Equals(arrow::ChunkedArray$create(
      value
    )))
  })
  testthat::expect_identical(
    parquet$nested$type$value_type$ToString(),
    "decimal128(20, 2)"
  )
  empty <- dplyr::tbl(
    con,
    dbplyr::sql(paste0("SELECT * FROM (", sql, ") invented WHERE FALSE")),
    vars = columns
  )
  testthat::expect_identical(
    nrow(collect_saved(empty, snapshot = "empty", dir = directory)),
    0L
  )
  empty_file <- list.files(
    directory,
    pattern = "^snapshot-empty-",
    full.names = TRUE
  )
  empty_parquet <- arrow::read_parquet(empty_file, as_data_frame = FALSE)
  testthat::expect_true(parquet$schema$Equals(
    empty_parquet$schema,
    check_metadata = FALSE
  ))
})
