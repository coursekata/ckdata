test_that("unbound results wait for parameters and can be cleared or disconnected", {
  fixture <- local_native_connection()
  result <- DBI::dbSendQuery(fixture$con, "SELECT ? AS value")
  expect_length(fixture$calls$queries, 0L)
  expect_true(DBI::dbIsValid(result))
  expect_false(DBI::dbHasCompleted(result))
  expect_equal(DBI::dbGetRowCount(result), 0)
  expect_error(DBI::dbFetch(result), "bind query parameters")
  expect_error(DBI::dbColumnInfo(result), "bind query parameters")
  expect_identical(DBI::dbGetStatement(result), "SELECT ? AS value")
  expect_true(DBI::dbClearResult(result))
  result <- DBI::dbSendQuery(fixture$con, "SELECT ? AS value")
  expect_warning(DBI::dbDisconnect(fixture$con), "uncleared")
  expect_false(DBI::dbIsValid(result))
})

test_that("binding preserves SQL, handles quotes and comments, and supports rebinding", {
  fixture <- local_native_connection()
  sql <- "SELECT ? AS value, '?' AS literal /* ? */ -- ?\n"
  result <- DBI::dbSendQuery(fixture$con, sql)
  withr::defer(DBI::dbClearResult(result))
  expect_identical(withVisible(DBI::dbBind(result, list("O'Brien")))$value, result)
  expect_identical(fixture$calls$queries, sql)
  expect_identical(fixture$calls$parameters, list("'O''Brien'"))
  expect_equal(nrow(DBI::dbFetch(result, 1)), 1L)
  DBI::dbBind(result, list(c("", NA_character_)))
  expect_equal(DBI::dbGetRowCount(result), 0)
  expect_equal(nrow(DBI::dbFetch(result)), 4L)
  expect_identical(fixture$calls$parameters, list("'O''Brien'", "''", "CAST(NULL AS VARCHAR)"))
  DBI::dbBind(result, list(integer()))
  empty <- DBI::dbFetch(result)
  expect_equal(nrow(empty), 0L)
  expect_s3_class(empty$big, "integer64")
  expect_true(DBI::dbHasCompleted(result))
  expect_true(tail(fixture$calls$schema_only, 1))
  DBI::dbBind(result, list(2L))
  expect_equal(nrow(DBI::dbFetch(result)), 2L)
})

test_that("parameter discovery handles empty comments without changing submitted SQL", {
  fixture <- local_native_connection()
  sql <- paste0(
    "SELECT /**/? AS first, --\n? AS second, ",
    "'/**/ -- ?named' AS literal, \"--?named/**/\" AS \"?named\""
  )
  result <- DBI::dbSendQuery(fixture$con, sql)
  withr::defer(DBI::dbClearResult(result))
  expect_length(fixture$calls$queries, 0L)
  DBI::dbBind(result, list(1L, "O'Brien"))
  expect_identical(fixture$calls$queries, sql)
  expect_identical(fixture$calls$parameters, list(c("1", "'O''Brien'")))
  expect_equal(nrow(DBI::dbFetch(result)), 2L)
  expect_error(DBI::dbSendQuery(fixture$con, "SELECT /**/ ?named"), "positional")
  expect_error(DBI::dbSendQuery(fixture$con, "SELECT --\n?named"), "positional")
  expect_length(fixture$calls$queries, 1L)
})

test_that("SQL with empty comments and no placeholders executes immediately", {
  fixture <- local_native_connection()
  sql <- "SELECT 1 /**/ AS value"
  result <- DBI::dbSendQuery(fixture$con, sql)
  withr::defer(DBI::dbClearResult(result))
  expect_identical(fixture$calls$queries, sql)
  expect_identical(fixture$calls$parameters, list(character()))
  expect_equal(nrow(DBI::dbFetch(result)), 2L)
})

test_that("invalid bindings submit nothing and leave the previous cursor available", {
  fixture <- local_native_connection()
  con <- fixture$con
  result <- DBI::dbSendQuery(con, "SELECT ? AS a, ? AS b", params = list(1L, 2L))
  withr::defer(DBI::dbClearResult(result))
  invalid <- list(
    list(list(1), "params must have one vector per placeholder"),
    list(list(1, 2, 3), "params must have one vector per placeholder"),
    list(list(a = 1, b = 2), "positional parameters must be unnamed"),
    list(list(1:2, 1), "parameter vectors must have equal lengths"),
    list(list(list(1), 1), "binary parameters must contain raw vectors or NULL"),
    list(list(DBI::SQL("1"), 2), "unsupported parameter type")
  )
  purrr::walk(invalid, function(case) {
    error <- rlang::catch_cnd(DBI::dbBind(result, case[[1]]), classes = "error")
    if (inherits(error, "purrr_error_indexed")) {
      expect_identical(error$location, 1L)
      error <- error$parent
    }
    expect_s3_class(error, "rlang_error")
    expect_match(rlang::cnd_message(error), case[[2]], fixed = TRUE)
  })
  expect_length(fixture$calls$queries, 1L)
  expect_equal(nrow(DBI::dbFetch(result)), 2L)
  expect_error(DBI::dbGetQuery(con, "SELECT ? AS a", params = list()), "one vector")
  expect_length(as.list(con@state$results), 1L)
  plain <- DBI::dbSendQuery(con, "SELECT 1 AS a")
  expect_error(DBI::dbBind(plain, list(1)), "no parameter")
  DBI::dbClearResult(plain)
  expect_error(DBI::dbSendQuery(con, "SELECT 1", params = list(1)), "no parameter placeholders")
})

test_that("parameters encode typed NULLs and exact scalar values", {
  con <- local_native_connection()$con
  expect_identical(parameter_literals(con, list(as.raw(c(0, 255)), NULL)), c("X'00ff'", "CAST(NULL AS VARBINARY)"))
  cases <- list(
    list(c(TRUE, FALSE, NA), c("TRUE", "FALSE", "CAST(NULL AS BOOLEAN)")),
    list(c(1L, NA_integer_), c("1", "CAST(NULL AS INTEGER)")),
    list(c(1, NA_real_, NaN, Inf), c("CAST(1 AS DOUBLE)", "CAST(NULL AS DOUBLE)", "CAST('NaN' AS DOUBLE)", "CAST('Infinity' AS DOUBLE)")),
    list(bit64::as.integer64(c("9007199254740993", NA)), c("CAST(9007199254740993 AS BIGINT)", "CAST(NULL AS BIGINT)")),
    list(as.Date(c("1960-01-02", NA)), c("DATE '1960-01-02'", "CAST(NULL AS DATE)")),
    list(as.POSIXct(c("2026-11-03 12:34:56.123", NA), tz = "UTC"), c("TIMESTAMP '2026-11-03 12:34:56.123'", "CAST(NULL AS TIMESTAMP)")),
    list(factor(c("a", NA)), c("'a'", "CAST(NULL AS VARCHAR)"))
  )
  for (case in cases) expect_identical(parameter_literals(con, case[[1]]), case[[2]])
})

test_that("Arrow queries bind parameters and retain owned streams after rebinding", {
  fixture <- local_native_connection()
  result <- DBI::dbSendQueryArrow(fixture$con, "SELECT ? AS a")
  expect_error(DBI::dbFetchArrow(result), "bind query parameters")
  DBI::dbBind(result, data.frame(a = 1:2))
  stream <- DBI::dbFetchArrow(result)
  DBI::dbBind(result, list(3L))
  DBI::dbClearResult(result)
  expect_equal(arrow::as_arrow_table(stream)$num_rows, 4)
  stream <- DBI::dbGetQueryArrow(fixture$con, "SELECT ? AS a", params = list(1))
  expect_equal(arrow::as_arrow_table(stream)$num_rows, 2)
})

test_that("Arrow binding preserves integer minima, exact BIGINT values and typed empty streams", {
  fixture <- local_native_connection()
  withr::local_options(arrow.int64_downcast = TRUE)
  params <- arrow::Table$create(
    first = arrow::Array$create(c("-2147483648", NA, "2147483647"))$cast(arrow::int32()),
    second = arrow::Array$create(c("-9223372036854775808", "9007199254740993", NA))$cast(arrow::int64())
  )
  for (send in list(DBI::dbSendQuery, DBI::dbSendQueryArrow)) {
    result <- send(fixture$con, "SELECT ? AS first, ? AS second", params = list(1L, 2L))
    expect_identical(DBI::dbFetch(result, 1)$id, 1L)
    stream <- nanoarrow::as_nanoarrow_array_stream(params)
    expect_identical(withVisible(DBI::dbBindArrow(result, stream))$value, result)
    expect_identical(
      tail(fixture$calls$parameters, 3),
      list(
        c("CAST('-2147483648' AS INTEGER)", "CAST('-9223372036854775808' AS BIGINT)"),
        c("CAST(NULL AS INTEGER)", "CAST('9007199254740993' AS BIGINT)"),
        c("CAST('2147483647' AS INTEGER)", "CAST(NULL AS BIGINT)")
      )
    )
    expect_true(DBI::dbIsValid(result))
    expect_equal(DBI::dbGetRowCount(result), 0)
    expect_equal(nrow(DBI::dbFetch(result)), 6L)
    empty <- nanoarrow::as_nanoarrow_array_stream(params$Slice(0, 0))
    DBI::dbBindArrow(result, empty)
    expect_identical(tail(fixture$calls$parameters, 1), list(c("CAST(NULL AS INTEGER)", "CAST(NULL AS BIGINT)")))
    expect_true(tail(fixture$calls$schema_only, 1))
    expect_equal(nrow(DBI::dbFetch(result)), 0L)
    expect_true(DBI::dbHasCompleted(result))
    DBI::dbClearResult(result)
  }
})

test_that("Arrow binding preserves decimals and unsigned integers before R conversion", {
  fixture <- local_native_connection()
  result <- DBI::dbSendQuery(fixture$con, "SELECT ? AS first, ? AS second, ? AS third")
  withr::defer(DBI::dbClearResult(result))
  params <- arrow::Table$create(
    first = arrow::Array$create(c("12345678901234567890123456789.123456789", NA))$cast(arrow::decimal128(38, 9)),
    second = arrow::Array$create(c("18446744073709551615", NA))$cast(arrow::uint64()),
    third = arrow::Array$create(c("12345678901234567890123456789012345678", NA))$cast(arrow::decimal128(38, 0))
  )
  DBI::dbBindArrow(result, nanoarrow::as_nanoarrow_array_stream(params))
  expect_identical(fixture$calls$parameters, list(
    c(
      "CAST('12345678901234567890123456789.123456789' AS DECIMAL(38, 9))",
      "CAST('18446744073709551615' AS DECIMAL(20, 0))",
      "CAST('12345678901234567890123456789012345678' AS DECIMAL(38, 0))"
    ),
    c("CAST(NULL AS DECIMAL(38, 9))", "CAST(NULL AS DECIMAL(20, 0))", "CAST(NULL AS DECIMAL(38, 0))")
  ))
  expect_identical(DBI::dbFetch(result, 1)$id, 1L)
  unsupported <- arrow::Table$create(
    first = arrow::Array$create("1.00")$cast(arrow::decimal256(50, 2)),
    second = arrow::Array$create("1")$cast(arrow::uint64()),
    third = arrow::Array$create("1")$cast(arrow::decimal128(38, 0))
  )
  expect_error(DBI::dbBindArrow(result, nanoarrow::as_nanoarrow_array_stream(unsupported)), "unsupported Arrow parameter type")
  expect_length(fixture$calls$queries, 2L)
  expect_equal(DBI::dbGetRowCount(result), 1)
  expect_identical(DBI::dbFetch(result)$id, c(2L, 1L, 2L))
})

test_that("a failed later execution does not expose partial concatenated results", {
  fixture <- local_native_connection()
  query <- native_query
  testthat::local_mocked_bindings(native_query = function(object, sql, timeout, parameters, schema_only) {
    if (rlang::is_string(parameters, "2")) rlang::abort("synthetic failure")
    query(object, sql, timeout, parameters, schema_only)
  }, .package = "ckdata")
  result <- DBI::dbSendQuery(fixture$con, "SELECT ? AS a", params = list(3L))
  withr::defer(DBI::dbClearResult(result))
  expect_identical(DBI::dbFetch(result, 1)$id, 1L)
  expect_error(DBI::dbBind(result, list(1:2)), "synthetic failure")
  expect_equal(DBI::dbGetRowCount(result), 1)
  expect_identical(DBI::dbFetch(result)$id, 2L)
  expect_true(DBI::dbHasCompleted(result))
  DBI::dbBind(result, list(4L))
  expect_equal(nrow(DBI::dbFetch(result)), 2L)
})

test_that("rebinding commits the replacement cursor before clearing the previous cursor", {
  fixture <- local_native_connection()
  result <- DBI::dbSendQuery(fixture$con, "SELECT ? AS value", params = list(1L))
  withr::defer(DBI::dbClearResult(result))
  expect_identical(DBI::dbFetch(result, 1)$id, 1L)
  previous <- result@state$handle
  published <- NULL
  clear <- native_result_clear
  testthat::local_mocked_bindings(native_result_clear = function(handle) {
    if (identical(handle, previous)) {
      published <<- result@state$handle
      clear(handle)
      rlang::interrupt()
      return(invisible(NULL))
    }
    clear(handle)
  }, .package = "ckdata")
  outcome <- rlang::try_fetch(
    {
      DBI::dbBind(result, list(2L))
      Sys.sleep(0.01)
    },
    interrupt = identity
  )
  expect_s3_class(outcome, "interrupt")
  expect_false(identical(published, previous))
  expect_identical(result@state$handle, published)
  expect_false(native_result_info(previous)$open)
  expect_true(DBI::dbIsValid(result))
  expect_equal(DBI::dbGetRowCount(result), 0)
  expect_identical(DBI::dbFetch(result)$id, 1:2)
})

test_that("native concatenation rejects aliases and fetched cursors", {
  a <- native_probe_result(FALSE)
  b <- native_probe_result(FALSE)
  expect_error(native_result_append(a, a), "itself")
  native_result_fetch(a, 1)
  expect_error(native_result_append(a, b), "partially fetched")
  native_result_clear(a)
  native_result_clear(b)
})

test_that("dbGetQuery consumes query options before fetching and clears results", {
  fixture <- local_native_connection()
  con <- fixture$con
  rows <- DBI::dbGetQuery(con, "SELECT ? AS value", params = list(1L), n = 1, immediate = FALSE, row.names = NULL)
  expect_identical(rows$id, 1L)
  expect_identical(fixture$calls$parameters, list("1"))
  expect_length(as.list(con@state$results), 0L)
  expect_equal(nrow(DBI::dbGetQuery(con, "SELECT synthetic", immediate = TRUE)), 2L)
  expect_error(DBI::dbGetQuery(con, "SELECT ? AS value", params = list(2L), n = -2), "n must be")
  expect_length(as.list(con@state$results), 0L)
})

test_that("binding factors warns before conversion and time values honor difftime units", {
  fixture <- local_native_connection()
  result <- DBI::dbSendQuery(fixture$con, "SELECT 1 AS value WHERE ? IS NOT NULL")
  withr::defer(DBI::dbClearResult(result))
  expect_warning(DBI::dbBind(result, list(factor("invented"))), "factor")
  expect_identical(tail(fixture$calls$parameters, 1)[[1]], "'invented'")
  times <- c(secs = "00:00:01", mins = "00:01:00", hours = "01:00:00")
  purrr::iwalk(times, function(time, units) {
    value <- as.difftime(c(1, NA), units = units)
    DBI::dbBind(result, list(value))
    expect_identical(tail(fixture$calls$parameters, 2), list(paste0("TIME '", time, "'"), "CAST(NULL AS TIME)"))
  })
  expect_identical(parameter_literals(fixture$con, as.difftime(1.123456, units = "secs")), "TIME '00:00:01.123456'")
})
