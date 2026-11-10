test_that("DBI pages one native result and zero-row fetches do not advance it", {
  fixture <- local_native_connection()
  con <- fixture$con
  result <- DBI::dbSendQuery(con, "SELECT synthetic")
  expect_equal(DBI::dbGetRowCount(result), 0)
  empty <- DBI::dbFetch(result, 0)
  expect_equal(nrow(empty), 0L)
  expect_s3_class(empty$big, "integer64")
  expect_equal(DBI::dbGetRowCount(result), 0)
  first <- DBI::dbFetch(result, 1)
  expect_identical(first$id, 1L)
  expect_identical(as.character(first$big), "9007199254740993")
  expect_true(is.na(first$s_null))
  expect_identical(first$s_empty, "")
  # Compare milliseconds exactly; relative equality on epoch values can hide lost precision.
  expect_identical(round(as.numeric(first$ts) * 1000), 1792154096789)
  expect_false(DBI::dbHasCompleted(result))
  expect_identical(DBI::dbFetch(result, Inf)$id, 2L)
  expect_true(DBI::dbHasCompleted(result))
  expect_equal(DBI::dbGetRowCount(result), 2)
  expect_equal(DBI::dbFetch(result), first[0, ])
  expect_identical(DBI::dbGetStatement(result), "SELECT synthetic")
  expect_identical(DBI::dbColumnInfo(result)$name, names(first))
  expect_length(fixture$calls$queries, 1L)
  expect_true(withVisible(DBI::dbClearResult(result))$value)
  expect_false(DBI::dbIsValid(result))
  expect_error(DBI::dbFetch(result), "result is closed")
  expect_warning(DBI::dbClearResult(result), "already cleared")
})

test_that("invalid fetch sizes leave the cursor available", {
  con <- local_native_connection()$con
  result <- DBI::dbSendQuery(con, "SELECT synthetic")
  withr::defer(DBI::dbClearResult(result))
  for (n in list(-2, 0.5, NaN, -Inf, "1", numeric(), c(1, 2), TRUE)) {
    expect_error(DBI::dbFetch(result, n), "n must be")
    expect_equal(DBI::dbGetRowCount(result), 0)
  }
  expect_equal(nrow(DBI::dbFetch(result, NA)), 2L)
})

test_that("column metadata retains every type before, during and after fetching", {
  con <- local_native_connection()$con
  expected <- data.frame(
    name = c("id", "s_null", "s_empty", "big", "ts"),
    type = c("Int32", "Utf8", "Utf8", "Int64", "Timestamp(ms)")
  )
  for (empty in c(FALSE, TRUE)) {
    result <- new_result(
      con,
      "invented",
      list(handle = native_probe_result(empty))
    )
    for (n in c(0, 1, -1)) {
      expect_identical(DBI::dbColumnInfo(result), expected)
      DBI::dbFetch(result, n)
    }
    expect_identical(DBI::dbColumnInfo(result), expected)
    DBI::dbClearResult(result)
  }
})

test_that("native catalog metadata includes partition columns in order", {
  info <- native_probe_metadata()
  expect_identical(info$name, c("id", "s_null", "s_empty", "big", "ts"))
  expect_identical(
    info$data_type,
    c("integer", "varchar", "varchar", "bigint", "timestamp")
  )
  fixture <- local_native_connection()
  expect_identical(DBI::dbListFields(fixture$con, "types"), info$name)
  expect_length(fixture$calls$queries, 0L)
})

test_that("dbGetQuery uses DBI's fetch and always clears the result", {
  fixture <- local_native_connection()
  expect_equal(
    nrow(DBI::dbGetQuery(fixture$con, "SELECT synthetic", n = 1)),
    1L
  )
  expect_length(as.list(fixture$con@state$results), 0L)
  testthat::local_mocked_bindings(
    stream_frame = function(stream) rlang::abort("conversion failed"),
    .package = "ckdata"
  )
  expect_error(
    DBI::dbGetQuery(fixture$con, "SELECT synthetic"),
    "conversion failed"
  )
  expect_length(as.list(fixture$con@state$results), 0L)
})

test_that("independent results and exported Arrow streams own their data", {
  con <- local_native_connection()$con
  first <- DBI::dbSendQuery(con, "SELECT first")
  second <- DBI::dbSendQuery(con, "SELECT second")
  DBI::dbClearResult(first)
  gc()
  expect_equal(nrow(DBI::dbFetch(second)), 2L)
  DBI::dbClearResult(second)
  stream <- DBI::dbGetQueryArrow(con, "SELECT arrow")
  expect_length(as.list(con@state$results), 0L)
  DBI::dbDisconnect(con)
  gc()
  table <- arrow::as_arrow_table(stream)
  expect_equal(table$num_rows, 2)
  expect_equal(table$big$type$ToString(), "int64")
})

test_that("disconnect invalidates open results and does not support writes", {
  con <- local_native_connection()$con
  for (operation in list(
    function() DBI::dbBegin(con),
    function() DBI::dbCommit(con),
    function() DBI::dbRollback(con),
    function() DBI::dbExecute(con, "DELETE FROM anything")
  )) {
    expect_error(operation(), "read-only")
  }
  result <- DBI::dbSendQuery(con, "SELECT synthetic")
  expect_warning(DBI::dbDisconnect(con), "uncleared")
  expect_false(DBI::dbIsValid(con))
  expect_false(DBI::dbIsValid(result))
  expect_error(DBI::dbFetch(result), "closed")
  expect_error(DBI::dbSendQuery(con, "SELECT 1"), "closed")
})

test_that("quoting uses SQL conventions without changing strings that look like dates", {
  con <- local_native_connection()$con
  expect_identical(
    as.character(DBI::dbQuoteIdentifier(con, c('a"b', "order", "a.b"))),
    c('"a""b"', '"order"', '"a.b"')
  )
  expect_identical(
    as.character(DBI::dbQuoteIdentifier(
      con,
      DBI::Id(schema = "s", table = "t")
    )),
    '"s"."t"'
  )
  expect_identical(
    as.character(DBI::dbQuoteString(
      con,
      c("O'Brien", "2026-11-03", NA_character_)
    )),
    c("'O''Brien'", "'2026-11-03'", "NULL")
  )
  expect_error(DBI::dbQuoteIdentifier(con, NA_character_), "cannot contain NA")
  expect_identical(DBI::dbQuoteIdentifier(con, DBI::SQL("x")), DBI::SQL("x"))
})

test_that("named-table discovery and SQL rendering do not fetch data", {
  fixture <- local_native_connection()
  tbl <- dplyr::tbl(fixture$con, DBI::Id(schema = "fixture", table = "types"))
  query <- tbl |>
    dplyr::filter(id > 1) |>
    dplyr::select(id, big)
  expect_equal(fixture$calls$metadata, 1L)
  expect_length(fixture$calls$queries, 0L)
  expect_output(dplyr::show_query(query), "SELECT")
  expect_length(fixture$calls$queries, 0L)
  expect_true(DBI::dbExistsTable(fixture$con, "types"))
  expect_false(DBI::dbExistsTable(fixture$con, "missing"))
  expect_equal(
    colnames(dplyr::tbl(fixture$con, dbplyr::in_schema("fixture", "types"))),
    colnames(tbl)
  )
  expect_equal(
    colnames(dplyr::tbl(fixture$con, I("fixture.types"))),
    colnames(tbl)
  )
  expect_length(fixture$calls$queries, 0L)
  raw <- dplyr::tbl(fixture$con, dbplyr::sql("SELECT synthetic"))
  expect_identical(colnames(raw), colnames(tbl))
  expect_length(fixture$calls$queries, 1L)
  supplied <- dplyr::tbl(
    fixture$con,
    dbplyr::sql("SELECT synthetic"),
    vars = c("id", "big")
  )
  expect_identical(colnames(supplied), c("id", "big"))
  expect_length(fixture$calls$queries, 1L)
})

test_that("catalog browsing and remote joins submit only the final collected query", {
  fixture <- local_native_connection()
  con <- fixture$con
  expect_identical(DBI::dbListTables(con), "types")
  expect_identical(
    DBI::dbListFields(con, "types"),
    c("id", "s_null", "s_empty", "big", "ts")
  )
  values <- dplyr::tbl(con, "types") |> dplyr::select(id, big)
  labels <- dplyr::tbl(con, "types") |> dplyr::select(id, s_empty)
  query <- dplyr::left_join(values, labels, by = "id") |> dplyr::filter(id > 0)
  expect_output(dplyr::show_query(query), "LEFT JOIN", fixed = TRUE)
  expect_length(fixture$calls$queries, 0L)

  collected <- dplyr::collect(query)
  expect_identical(collected$id, c(1L, 2L))
  expect_identical(
    fixture$calls$queries,
    as.character(dbplyr::remote_query(query))
  )
})

test_that("empty and populated native results have identical R column classes", {
  populated <- stream_frame(native_result_fetch(native_probe_result(FALSE), -1))
  empty <- stream_frame(native_result_fetch(native_probe_result(TRUE), -1))
  expect_equal(empty, populated[0, ])
})

test_that("foreign or serialized external pointers are rejected before dereferencing", {
  stream <- native_result_fetch(native_probe_result(FALSE), -1)
  for (object in list(NULL, new("externalptr"), stream, 0, "1")) {
    expect_error(native_result_info(object), "invalid ckdata native handle")
  }
  restored <- unserialize(serialize(native_probe_result(FALSE), NULL))
  expect_error(native_result_info(restored), class = "error")
})

test_that("an R interrupt cancels and drains a real native asynchronous job", {
  job <- native_probe_job(pending = TRUE)
  cleanup <- local_native_cancellation()
  ready <- native_ready
  calls <- 0L
  testthat::local_mocked_bindings(
    native_ready = function(object) {
      calls <<- calls + 1L
      if (calls == 2L) {
        rlang::interrupt()
      }
      ready(object)
    },
    .package = "ckdata"
  )
  interrupted <- rlang::try_fetch(native_wait(job), interrupt = identity)
  expect_s3_class(interrupted, "interrupt")
  expect_identical(cleanup$cancelled, list(job))
  expect_s3_class(cleanup$terminal_error, "ckdata_native_error")
  expect_match(
    rlang::cnd_message(cleanup$terminal_error),
    "synthetic query cancelled",
    fixed = TRUE
  )
  expect_true(ready(job))
  expect_error(native_value(job), "already consumed")
})

test_that("runtime shutdown drains cancellation and allows a fresh runtime", {
  job <- native_probe_job(pending = TRUE)
  native_shutdown()
  expect_true(native_ready(job))
  expect_error(native_value(job), "cancelled")
  value <- native_wait(native_probe_job(pending = FALSE))
  expect_equal(native_result_info(value$handle)$rows, 2)
  native_result_clear(value$handle)
})

test_that("DBI type descriptions use Athena types for supported R values", {
  con <- local_native_connection()$con
  values <- list(
    text = "invented",
    flag = TRUE,
    integer = 1L,
    number = 1.25,
    big = bit64::as.integer64("9007199254740993"),
    factor = factor("a"),
    day = as.Date("2026-11-04"),
    stamp = as.POSIXct("2026-11-04", tz = "UTC"),
    bytes = list(as.raw(c(0, 255)))
  )
  expected <- c(
    text = "VARCHAR",
    flag = "BOOLEAN",
    integer = "INTEGER",
    number = "DOUBLE",
    big = "BIGINT",
    factor = "VARCHAR",
    day = "DATE",
    stamp = "TIMESTAMP",
    bytes = "VARBINARY"
  )
  for (object in list(ckdata(), con)) {
    expect_identical(
      vapply(values, function(x) DBI::dbDataType(object, x), character(1)),
      expected
    )
    expect_identical(
      DBI::dbDataType(object, data.frame(text = "x", flag = TRUE)),
      expected[c("text", "flag")]
    )
    expect_identical(DBI::dbDataType(object, raw()), "VARBINARY")
  }
})

test_that("DBI literal quoting shares Athena binding encodings", {
  con <- local_native_connection()$con
  expect_identical(
    as.character(DBI::dbQuoteLiteral(con, c(TRUE, FALSE, NA))),
    c("TRUE", "FALSE", "CAST(NULL AS BOOLEAN)")
  )
  expect_identical(
    as.character(DBI::dbQuoteLiteral(con, c(Inf, -Inf, NaN, NA_real_))),
    c(
      "CAST('Infinity' AS DOUBLE)",
      "CAST('-Infinity' AS DOUBLE)",
      "CAST('NaN' AS DOUBLE)",
      "CAST(NULL AS DOUBLE)"
    )
  )
  expect_identical(
    as.character(DBI::sqlInterpolate(con, "WHERE flag = ?value", value = TRUE)),
    "WHERE flag = TRUE"
  )
  expect_identical(
    as.character(DBI::dbQuoteLiteral(
      con,
      bit64::as.integer64("9007199254740993")
    )),
    "CAST(9007199254740993 AS BIGINT)"
  )
  named <- DBI::dbQuoteLiteral(
    con,
    c(first = "O'Brien", missing = NA_character_)
  )
  expect_named(named, c("first", "missing"))
  expect_identical(
    unname(as.character(named)),
    c("'O''Brien'", "CAST(NULL AS VARCHAR)")
  )
  expect_identical(
    as.character(DBI::dbQuoteLiteral(con, list(as.raw(c(0, 255)), NULL))),
    c("X'00ff'", "NULL")
  )
  expect_identical(
    DBI::dbQuoteLiteral(con, DBI::SQL("already_quoted")),
    DBI::SQL("already_quoted")
  )
})

test_that("empty supported literals preserve zero length", {
  con <- local_native_connection()$con
  values <- list(
    numeric(),
    integer(),
    logical(),
    character(),
    bit64::integer64(),
    as.Date(character()),
    as.POSIXct(character()),
    factor(),
    list(),
    as.difftime(numeric(), units = "secs")
  )
  for (value in values) {
    expect_length(DBI::dbQuoteLiteral(con, value), 0L)
  }
})

test_that("quoted table identifiers are decoded before catalog lookup", {
  fixture <- local_native_connection()
  con <- fixture$con
  for (name in list(
    "types",
    DBI::Id(schema = "other_fixture", table = "types")
  )) {
    expected_schema <- if (is.character(name)) "fixture" else "other_fixture"
    quoted <- DBI::dbQuoteIdentifier(con, name)
    expect_true(DBI::dbExistsTable(con, quoted))
    expect_identical(
      tail(fixture$calls$metadata_args, 1)[[1]],
      list(schema = expected_schema, table = NULL)
    )
    expected <- list(schema = expected_schema, table = "types")
    direct <- DBI::dbListFields(con, name)
    expect_identical(tail(fixture$calls$metadata_args, 1)[[1]], expected)
    expect_identical(DBI::dbListFields(con, quoted), direct)
    expect_identical(tail(fixture$calls$metadata_args, 1)[[1]], expected)
  }
})

test_that("dbReadTable reuses standard row-name and column-name handling", {
  con <- local_native_connection()$con
  for (row_names in list(FALSE, NULL)) {
    expect_equal(
      nrow(DBI::dbReadTable(con, "types", row.names = row_names)),
      2L
    )
  }
  convert <- stream_frame
  testthat::local_mocked_bindings(
    stream_frame = function(stream) {
      frame <- convert(stream)
      names(frame)[1:2] <- c("with spaces", "row_names")
      frame$row_names <- c("first", "second")
      frame
    },
    .package = "ckdata"
  )
  raw <- DBI::dbReadTable(con, "types", check.names = FALSE)
  expect_named(raw, c("with spaces", "row_names", "s_empty", "big", "ts"))
  repaired <- DBI::dbReadTable(con, "types", check.names = TRUE)
  expect_named(repaired, make.names(names(raw), unique = TRUE))
  named <- DBI::dbReadTable(con, "types", row.names = TRUE, check.names = FALSE)
  expect_identical(rownames(named), c("first", "second"))
  expect_false("row_names" %in% names(named))
})

test_that("Arrow chunks use the native cursor and expose standard result info", {
  con <- local_native_connection()$con
  result <- DBI::dbSendQueryArrow(con, "SELECT synthetic")
  withr::defer(DBI::dbClearResult(result))
  for (i in seq_len(130)) {
    native_result_append(result@state$handle, native_probe_result(FALSE))
  }
  expect_identical(DBI::dbGetInfo(result)$statement, "SELECT synthetic")
  expect_equal(DBI::dbGetInfo(result)$row.count, 0)
  first <- DBI::dbFetchArrowChunk(result)
  expect_s3_class(first, "nanoarrow_array")
  expect_equal(first$length, 256L)
  expect_equal(DBI::dbGetRowCount(result), 256)
  expect_false(DBI::dbHasCompleted(result))
  second <- DBI::dbFetchArrowChunk(result)
  expect_equal(second$length, 6L)
  expect_true(DBI::dbHasCompleted(result))
  empty <- DBI::dbFetchArrowChunk(result)
  expect_equal(empty$length, 0L)
  expect_true(arrow::as_arrow_array(first)$type$Equals(
    arrow::as_arrow_array(empty)$type
  ))
  frame <- stream_frame(nanoarrow::as_nanoarrow_array_stream(first))
  expect_identical(as.character(frame$big[1]), "9007199254740993")
})

test_that("elapsed time-limit errors cancel and drain native work", {
  job <- native_probe_job(pending = TRUE)
  cleanup <- local_native_cancellation()
  withr::defer({
    setTimeLimit(cpu = Inf, elapsed = Inf, transient = FALSE)
    native_cancel(job)
    native_shutdown()
  })
  setTimeLimit(elapsed = 0.05, transient = TRUE)
  outcome <- rlang::try_fetch(native_wait(job), error = identity)
  setTimeLimit(cpu = Inf, elapsed = Inf, transient = FALSE)
  expect_s3_class(outcome, "simpleError")
  expect_false(inherits(outcome, "ckdata_native_error"))
  expect_identical(cleanup$cancelled, list(job))
  expect_s3_class(cleanup$terminal_error, "ckdata_native_error")
  expect_match(
    rlang::cnd_message(cleanup$terminal_error),
    "synthetic query cancelled",
    fixed = TRUE
  )
  expect_true(native_ready(job))
  expect_error(native_value(job), "already consumed")
})

test_that("catalog discovery honors schema prefixes and returns usable identifiers", {
  fixture <- local_native_connection()
  con <- fixture$con
  prefixes <- list(
    DBI::Id(schema = "other_fixture"),
    DBI::dbQuoteIdentifier(con, DBI::Id(schema = "other_fixture"))
  )
  for (prefix in prefixes) {
    objects <- DBI::dbListObjects(con, prefix = prefix)
    expect_identical(
      tail(fixture$calls$metadata_args, 1)[[1]]$schema,
      "other_fixture"
    )
    expect_identical(
      objects$table[[1]],
      DBI::Id(schema = "other_fixture", table = "types")
    )
    expect_false(objects$is_prefix[[1]])
    expect_true(DBI::dbExistsTable(con, objects$table[[1]]))
  }
  objects <- DBI::dbListObjects(
    con,
    prefix = DBI::Id(catalog = "AwsDataCatalog")
  )
  expect_identical(
    objects$table[[1]],
    DBI::Id(catalog = "AwsDataCatalog", schema = "fixture", table = "types")
  )
  expect_equal(
    nrow(DBI::dbListObjects(
      con,
      prefix = DBI::Id(schema = "fixture", table = "missing")
    )),
    0L
  )
})

test_that("Arrow table reading and parameter streams follow DBI contracts", {
  fixture <- local_native_connection()
  con <- fixture$con
  table <- arrow::as_arrow_table(DBI::dbReadTableArrow(
    con,
    DBI::Id(schema = "fixture", table = "types")
  ))
  expect_equal(table$num_rows, 2)
  expect_identical(table$big$type$ToString(), "int64")
  result <- DBI::dbSendQuery(con, "SELECT ? AS value")
  withr::defer(DBI::dbClearResult(result))
  params <- data.frame(1:2)
  names(params) <- ""
  DBI::dbBindArrow(result, nanoarrow::as_nanoarrow_array_stream(params))
  expect_equal(nrow(DBI::dbFetch(result)), 4L)
  expect_identical(
    tail(fixture$calls$parameters, 2),
    list("CAST('1' AS INTEGER)", "CAST('2' AS INTEGER)")
  )
})

test_that("quoted catalog prefixes preserve their namespace components", {
  fixture <- local_native_connection()
  con <- fixture$con
  prefixes <- list(
    DBI::Id(catalog = "AwsDataCatalog"),
    DBI::Id(catalog = "AwsDataCatalog", schema = "other_fixture"),
    DBI::Id(
      catalog = "AwsDataCatalog",
      schema = "other_fixture",
      table = "types"
    ),
    DBI::Id(schema = "other_fixture", table = "types")
  )
  expected <- list(
    DBI::Id(catalog = "AwsDataCatalog", schema = "fixture", table = "types"),
    DBI::Id(
      catalog = "AwsDataCatalog",
      schema = "other_fixture",
      table = "types"
    ),
    DBI::Id(
      catalog = "AwsDataCatalog",
      schema = "other_fixture",
      table = "types"
    ),
    DBI::Id(schema = "other_fixture", table = "types")
  )
  purrr::walk2(prefixes, expected, function(prefix, id) {
    direct <- DBI::dbListObjects(con, prefix)
    expect_identical(
      tail(fixture$calls$metadata_args, 1)[[1]],
      list(schema = id@name[["schema"]], table = NULL)
    )
    quoted <- DBI::dbListObjects(con, DBI::dbQuoteIdentifier(con, prefix))
    expect_identical(
      tail(fixture$calls$metadata_args, 1)[[1]],
      list(schema = id@name[["schema"]], table = NULL)
    )
    expect_identical(direct$table, I(list(id)))
    expect_identical(quoted$table, I(list(id)))
    expect_true(DBI::dbExistsTable(con, quoted$table[[1]]))
  })
})
