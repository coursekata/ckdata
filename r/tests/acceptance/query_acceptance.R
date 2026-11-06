# Explicit synthetic-only acceptance; source credentials must be able to
# assume the selected fixture reader. No default workgroup or data table.
args <- commandArgs(trailingOnly = TRUE)
if (length(args) != 3L || args[[1]] != "--run-live" || !args[[3]] %in% c("internal", "research")) {
  rlang::abort(
    "No AWS clients created. Usage: Rscript --vanilla query_acceptance.R --run-live DEPLOYMENT.json internal|research",
    class = "ckdata_acceptance_usage"
  )
}
seat_name <- args[[3]]
script <- sub("^--file=", "", grep("^--file=", commandArgs(), value = TRUE))
source(file.path(dirname(script), "helper_acceptance.R"), local = TRUE)
deployment <- jsonlite::fromJSON(args[[2]], simplifyVector = FALSE)
seat <- validate_deployment(deployment, seat_name)
account <- deployment$account_id
expected_role <- paste0("ck-datalake-tst-ckdata-", seat_name, "-reader")

library(DBI)
library(ckdata)
fence <- function(connection) {
  info <- DBI::dbGetInfo(connection)
  expected <- list(account = account, schema = deployment$database, workgroup = seat$workgroup)
  purrr::iwalk(expected, function(value, field) {
    if (!identical(info[[field]], value)) {
      cli::cli_abort("Connection {.field {field}} does not match the synthetic fixture contract.", class = "ckdata_acceptance_identity")
    }
  })
  role_prefix <- paste0("arn:aws:sts::", account, ":assumed-role/", expected_role, "/")
  if (!rlang::is_string(info$arn) || !startsWith(info$arn, role_prefix)) {
    cli::cli_abort("Connection ARN must identify the selected synthetic fixture reader.", class = "ckdata_acceptance_identity")
  }
  namespace <- rlang::ns_env("ckdata")
  configuration <- rlang::env_get(namespace, "native_wait")(
    rlang::env_get(namespace, "native_workgroup")(connection@state$handle)
  )
  validate_workgroup(configuration, seat, account)
}

probe <- r"(SELECT * FROM (VALUES (1, CAST(NULL AS VARCHAR), '', BIGINT '9007199254740993', DECIMAL '1.10', TIMESTAMP '2026-10-16 12:34:56.789', ARRAY[1, 2]), (2, 'a', 'b', NULL, NULL, NULL, NULL)) AS t(id, s_null, s_empty, big, dec, ts, arr))"

scalar_probe <- r"(SELECT
TRUE AS flag,
CAST(7 AS TINYINT) AS tiny,
CAST(32000 AS SMALLINT) AS small,
42 AS integer_value,
BIGINT '9007199254740993' AS big,
REAL '1.25' AS single_value,
DOUBLE '1.25' AS double_value,
DECIMAL '1.10' AS decimal_small,
CAST('12345678901234567890.123456789012345678' AS DECIMAL(38,18)) AS decimal_wide,
CAST('-12345678901234567890.123456789012345678' AS DECIMAL(38,18)) AS decimal_negative,
CAST('0' AS DECIMAL(38,18)) AS decimal_zero,
CAST(NULL AS DECIMAL(38,18)) AS decimal_null,
CAST('invented' AS VARCHAR) AS text_value,
CAST('xy123' AS CHAR(5)) AS fixed_value,
CAST('' AS VARCHAR) AS empty_text,
CAST(NULL AS VARCHAR) AS null_text,
X'00017FFF' AS bytes_value,
DATE '2026-10-16' AS day_value,
TIMESTAMP '2026-10-16 12:34:56.789' AS moment,
TIMESTAMP '1960-01-02 03:04:05.678' AS before_epoch)"

nested_probe <- r"(SELECT
ARRAY[BIGINT '9007199254740993', CAST(NULL AS BIGINT), BIGINT '-4'] AS array_nullable,
CAST(ARRAY[] AS ARRAY(INTEGER)) AS empty_array,
ARRAY[CAST('1.123456789012345678' AS DECIMAL(38,18)), CAST(NULL AS DECIMAL(38,18))] AS decimal_array,
MAP(ARRAY['a','b'], ARRAY[CAST('1.10' AS DECIMAL(38,18)), CAST(NULL AS DECIMAL(38,18))]) AS map_decimal,
CAST(ROW(BIGINT '9007199254740993','invented',ARRAY[1,CAST(NULL AS INTEGER),2]) AS ROW(id BIGINT,label VARCHAR,items ARRAY(INTEGER))) AS named_record,
CAST(ARRAY[ROW(BIGINT '3',ARRAY[1,CAST(NULL AS INTEGER)]),CAST(NULL AS ROW(id BIGINT,items ARRAY(INTEGER)))] AS ARRAY(ROW(id BIGINT,items ARRAY(INTEGER)))) AS record_array,
CAST(MAP(ARRAY['a','b'],ARRAY[ARRAY[ROW(DECIMAL '1.10',TIMESTAMP '2026-10-16 12:34:56.789')],CAST(NULL AS ARRAY(ROW(amount DECIMAL(10,2),stamp TIMESTAMP)))]) AS MAP(VARCHAR,ARRAY(ROW(amount DECIMAL(10,2),stamp TIMESTAMP)))) AS map_records,
CAST(ROW(ARRAY[ARRAY[1,CAST(NULL AS INTEGER)],CAST(NULL AS ARRAY(INTEGER)),CAST(ARRAY[] AS ARRAY(INTEGER))],MAP(ARRAY['a'],ARRAY[ARRAY[BIGINT '9007199254740993',CAST(NULL AS BIGINT)]])) AS ROW(matrix ARRAY(ARRAY(INTEGER)),lookup MAP(VARCHAR,ARRAY(BIGINT)))) AS deep_record,
CAST(NULL AS ROW(id BIGINT,items ARRAY(INTEGER))) AS null_record)"

arrow_decimal_matches <- function(column, values) {
  expected <- arrow::Array$create(values, type = arrow::utf8())$cast(arrow::decimal128(38, 18))
  if (inherits(column, "ChunkedArray")) expected <- arrow::ChunkedArray$create(expected)
  column$Equals(expected)
}
arrow_micros <- function(column) {
  withr::with_options(
    list(arrow.int64_downcast = FALSE),
    as.character(as.vector(column$cast(arrow::timestamp("us"))$cast(arrow::int64())))
  )
}

check_scalar_values <- function(table, frame) {
  testthat::expect_identical(frame$flag, TRUE)
  testthat::expect_identical(frame$tiny, 7L)
  testthat::expect_identical(frame$small, 32000L)
  testthat::expect_identical(frame$integer_value, 42L)
  testthat::expect_identical(as.character(frame$big), "9007199254740993")
  testthat::expect_identical(frame$single_value, 1.25)
  testthat::expect_identical(frame$double_value, 1.25)
  testthat::expect_identical(frame$decimal_small, 1.1)
  testthat::expect_identical(frame$text_value, "invented")
  testthat::expect_identical(frame$fixed_value, "xy123")
  testthat::expect_identical(frame$empty_text, "")
  testthat::expect_true(is.na(frame$null_text))
  testthat::expect_identical(frame$bytes_value[[1]], as.raw(c(0, 1, 127, 255)))
  testthat::expect_identical(frame$day_value, as.Date("2026-10-16"))
  testthat::expect_true(arrow_decimal_matches(table$decimal_wide, "12345678901234567890.123456789012345678"))
  testthat::expect_true(arrow_decimal_matches(table$decimal_negative, "-12345678901234567890.123456789012345678"))
  testthat::expect_true(arrow_decimal_matches(table$decimal_zero, "0"))
  testthat::expect_true(arrow_decimal_matches(table$decimal_null, NA_character_))
  testthat::expect_identical(arrow_micros(table$moment), "1792154096789000")
  testthat::expect_identical(arrow_micros(table$before_epoch), "-315521754322000")
}

check_nested_values <- function(table, frame) {
  nullable <- table$array_nullable$chunk(0)$values()
  record <- table$named_record$chunk(0)
  records <- table$record_array$chunk(0)$values()
  mapping <- table$map_decimal$chunk(0)
  map_records <- frame$map_records[[1]]
  testthat::expect_identical(as.character(frame$array_nullable[[1]]), c("9007199254740993", NA, "-4"))
  testthat::expect_identical(nullable$type$ToString(), "int64")
  testthat::expect_length(frame$empty_array[[1]], 0L)
  testthat::expect_true(arrow_decimal_matches(table$decimal_array$chunk(0)$values(), c("1.123456789012345678", NA)))
  testthat::expect_identical(as.vector(mapping$keys()), c("a", "b"))
  testthat::expect_true(arrow_decimal_matches(mapping$items(), c("1.10", NA)))
  testthat::expect_identical(as.character(frame$named_record$id), "9007199254740993")
  testthat::expect_identical(frame$named_record$label, "invented")
  testthat::expect_identical(frame$named_record$items[[1]], c(1L, NA, 2L))
  testthat::expect_identical(as.character(as.vector(record$GetFieldByName("id"))), "9007199254740993")
  testthat::expect_false(records$IsNull(0))
  testthat::expect_true(records$IsNull(1))
  testthat::expect_identical(as.character(frame$record_array[[1]]$id), c("3", NA))
  testthat::expect_identical(frame$record_array[[1]]$items[[1]], c(1L, NA))
  testthat::expect_identical(map_records$key, c("a", "b"))
  testthat::expect_null(map_records$value[[2]])
  testthat::expect_identical(map_records$value[[1]]$amount, 1.1)
  expect_timestamp_equal(map_records$value[[1]]$stamp, 1792154096.789)
  testthat::expect_identical(frame$deep_record$matrix[[1]][[1]], c(1L, NA))
  testthat::expect_null(frame$deep_record$matrix[[1]][[2]])
  testthat::expect_length(frame$deep_record$matrix[[1]][[3]], 0L)
  testthat::expect_identical(frame$deep_record$lookup[[1]]$key, "a")
  testthat::expect_identical(as.character(frame$deep_record$lookup[[1]]$value[[1]]), c("9007199254740993", NA))
  testthat::expect_true(all(is.na(frame$null_record$id)))
  testthat::expect_null(frame$null_record$items[[1]])
}

check_temporal_range <- function(con) {
  sql <- r"(SELECT id, stamp, day_value, ARRAY[stamp, CAST(NULL AS TIMESTAMP)] AS stamps,
CAST(ROW(stamp) AS ROW(stamp TIMESTAMP)) AS record_value,
MAP(ARRAY['a','b'], ARRAY[stamp, CAST(NULL AS TIMESTAMP)]) AS mapping
FROM (VALUES
(1, TIMESTAMP '1600-01-02 03:04:05.678', DATE '1600-01-02'),
(2, TIMESTAMP '1969-12-31 23:59:59.999', DATE '1969-12-31'),
(3, TIMESTAMP '2300-02-03 04:05:06.789', DATE '2300-02-03'),
(4, TIMESTAMP '9999-12-31 23:59:59.999', DATE '9999-12-31'),
(5, CAST(NULL AS TIMESTAMP), CAST(NULL AS DATE)),
(6, TIMESTAMP '0001-01-01 00:00:00.001', DATE '0001-01-01')) t(id, stamp, day_value))"
  table <- lake_query(sql, as = "arrow", con = con)
  expected <- c("-11675998554322000", "-1000", "10416657906789000", "253402300799999000", NA, "-62135596799999000")
  frame <- DBI::dbGetQuery(con, sql)
  frame <- frame[order(frame$id), ]
  actual <- data.frame(id = as.vector(table$id), stamp = arrow_micros(table$stamp))
  testthat::expect_identical(actual$stamp[order(actual$id)], expected)
  testthat::expect_identical(frame$day_value, as.Date(c("1600-01-02", "1969-12-31", "2300-02-03", "9999-12-31", NA, "0001-01-01")))
  expected_epochs <- c(-11675998554.322, -0.001, 10416657906.789, 253402300799.999, NA, -62135596799.999)
  expect_timestamp_equal(frame$stamp, expected_epochs)
  for (i in which(!is.na(frame$stamp))) {
    testthat::expect_identical(as.numeric(frame$stamps[[i]][1]), as.numeric(frame$stamp[i]))
    testthat::expect_true(is.na(frame$stamps[[i]][2]))
    testthat::expect_identical(as.numeric(frame$record_value$stamp[i]), as.numeric(frame$stamp[i]))
    testthat::expect_identical(frame$mapping[[i]]$key, c("a", "b"))
    testthat::expect_identical(as.numeric(frame$mapping[[i]]$value[1]), as.numeric(frame$stamp[i]))
    testthat::expect_true(is.na(frame$mapping[[i]]$value[2]))
  }
  empty <- lake_query(paste0("SELECT * FROM (", sql, ") q WHERE FALSE"), as = "arrow", con = con)
  testthat::expect_equal(empty$num_rows, 0, tolerance = 0)
  testthat::expect_true(table$schema$Equals(empty$schema, check_metadata = TRUE))
  cat("PASS historical/future/negative/fractional and nested timestamps, dates and typed empties\n")
}

check_nullable_scalars <- function(con) {
  sql <- r"(SELECT id, CAST(tiny AS TINYINT) AS tiny, CAST(small AS SMALLINT) AS small,
CAST(single_value AS REAL) AS single_value, flag, big
FROM (VALUES (1, -128, -32768, -1.25, TRUE, BIGINT '9007199254740993'),
(2, 127, 32767, 1.25, FALSE, BIGINT '9223372036854775807'),
(3, NULL, NULL, NULL, NULL, NULL)) t(id, tiny, small, single_value, flag, big))"
  result <- DBI::dbSendQuery(con, sql)
  withr::defer(DBI::dbClearResult(result))
  info <- DBI::dbColumnInfo(result)
  rows <- list(DBI::dbFetch(result, 1), DBI::dbFetch(result, 1), DBI::dbFetch(result))
  purrr::walk(rows, function(row) {
    testthat::expect_s3_class(row$big, "integer64")
    testthat::expect_type(row$tiny, "integer")
    testthat::expect_type(row$small, "integer")
    testthat::expect_type(row$flag, "logical")
  })
  frame <- dplyr::bind_rows(rows) |> dplyr::arrange(id)
  testthat::expect_identical(frame$tiny, c(-128L, 127L, NA))
  testthat::expect_identical(frame$small, c(-32768L, 32767L, NA))
  testthat::expect_identical(frame$single_value, c(-1.25, 1.25, NA))
  testthat::expect_identical(frame$flag, c(TRUE, FALSE, NA))
  testthat::expect_identical(as.character(frame$big), c("9007199254740993", "9223372036854775807", NA))
  testthat::expect_identical(DBI::dbColumnInfo(result), info)
  testthat::expect_true(DBI::dbHasCompleted(result))
  cat("PASS nullable narrow scalars, int64 boundaries, partial-fetch classes and column metadata\n")
}

check_binary_values <- function(con) {
  values <- list(NULL, raw(), as.raw(0:255))
  result <- DBI::dbSendQuery(con, "SELECT ? AS bytes", params = list(values))
  withr::defer(DBI::dbClearResult(result))
  pieces <- purrr::map(seq_along(values), function(i) DBI::dbFetch(result, 1)$bytes[[1]])
  testthat::expect_identical(pieces, values)
  testthat::expect_true(DBI::dbHasCompleted(result))
  table <- arrow::as_arrow_table(DBI::dbGetQueryArrow(con, "SELECT ? AS bytes", params = list(values)))
  testthat::expect_identical(table$bytes$type$ToString(), "binary")
  testthat::expect_identical(purrr::map(as.vector(table$bytes), identity), values)
  cat("PASS binary NULL versus empty bytes and all 256 byte values, binding, Arrow and paging\n")
}

check_inline_queries <- function(con) {
  values <- data.frame(
    id = c(1L, 2L, 99L, 100L, 101L, 102L), label = c("O'Brien", "", rep(NA_character_, 4)),
    flag = c(TRUE, FALSE, rep(NA, 4)), score = c(1.25, NA_real_, -4.5, Inf, -Inf, NaN),
    big = bit64::as.integer64(c("9007199254740993", NA, "-4", rep(NA_character_, 3))),
    day = as.Date(c("2026-11-03", NA, "1960-01-02", rep(NA_character_, 3))),
    stamp = as.POSIXct(c("2026-11-03 12:34:56.123", NA, "1960-01-02 03:04:05.678", rep(NA_character_, 3)), tz = "UTC")
  )
  inline <- dbplyr::copy_inline(con, values)
  actual <- dplyr::collect(inline) |> dplyr::arrange(id)
  testthat::expect_identical(actual$id, values$id)
  testthat::expect_identical(actual$label, values$label)
  testthat::expect_identical(actual$flag, values$flag)
  testthat::expect_identical(actual$score, c(1.25, NA_real_, -4.5, Inf, -Inf, NA_real_))
  testthat::expect_identical(as.character(actual$big), as.character(values$big))
  testthat::expect_identical(actual$day, values$day)
  expect_timestamp_equal(actual$stamp, values$stamp)
  empty <- dplyr::collect(dbplyr::copy_inline(con, values[0, ]))
  testthat::expect_identical(nrow(empty), 0L)
  testthat::expect_equal(empty, actual[0, ], ignore_attr = "tzone")
  nulls <- data.frame(text = NA_character_, number = NA_real_, integer = NA_integer_, flag = NA)
  actual <- dplyr::collect(dbplyr::copy_inline(con, nulls))
  testthat::expect_identical(nrow(actual), 1L)
  testthat::expect_type(actual$text, "character")
  testthat::expect_type(actual$number, "double")
  testthat::expect_type(actual$integer, "integer")
  testthat::expect_type(actual$flag, "logical")
  purrr::walk(actual, function(column) testthat::expect_true(all(is.na(column))))
  remote <- dplyr::tbl(con, "types") |> dplyr::select(id)
  joined <- dplyr::left_join(remote, dplyr::select(inline, id, label), by = "id") |>
    dplyr::collect() |>
    dplyr::arrange(id)
  direct <- dplyr::left_join(remote, values[c("id", "label")], by = "id", copy = "inline") |>
    dplyr::collect() |>
    dplyr::arrange(id)
  testthat::expect_identical(joined$id, c(1L, 2L))
  testthat::expect_identical(joined$label, c("O'Brien", ""))
  testthat::expect_identical(joined, direct)
  empty_join <- dplyr::inner_join(remote,
    dbplyr::copy_inline(con, values[0, c("id", "label")]),
    by = "id"
  ) |> dplyr::collect()
  testthat::expect_identical(nrow(empty_join), 0L)
  testthat::expect_equal(empty_join, joined[0, ])
  cat("PASS inline values, signed infinity/NaN-to-NULL, exact int64/date/time, typed NULLs/empties and read-only joins\n")
}

check_typed_missing <- function(con) {
  query <- dplyr::tbl(con, "types") |>
    dplyr::select(id) |>
    dplyr::mutate(
      text = NA_character_, integer = NA_integer_, number = NA_real_, flag = NA,
      day = !!as.Date(NA), stamp = !!as.POSIXct(NA), big = !!bit64::as.integer64(NA),
      inferred = id + NA, conditional = dplyr::if_else(id == 1, NA_integer_, 3L),
      parenthesized = (NA), coalesced = coalesce(NA, NA),
      logical_conditional = dplyr::if_else(id > 0, NA, NA),
      cases = dplyr::case_when(id > 0 ~ NA, .default = NA),
      nested_coalesce = coalesce(dplyr::if_else(id > 0, NA, NA), NA),
      nested_conditional = dplyr::if_else(id > 0, coalesce(NA, NA), NA), nan_missing = NaN,
      matched_flag = dplyr::case_match(id, 1L ~ NA, .default = NA),
      matched_nested = dplyr::case_match(1L ~ (NA), .x = id, .default = coalesce(NA, NA)),
      matched_number = dplyr::case_match(id, 1L ~ NA, .default = 3L)
    )
  rows <- dplyr::collect(query) |> dplyr::arrange(id)
  testthat::expect_identical(nrow(rows), 2L)
  testthat::expect_type(rows$text, "character")
  testthat::expect_type(rows$integer, "integer")
  testthat::expect_identical(rows$number, rep(NA_real_, 2L))
  testthat::expect_type(rows$flag, "logical")
  testthat::expect_s3_class(rows$day, "Date")
  testthat::expect_s3_class(rows$stamp, "POSIXct")
  testthat::expect_s3_class(rows$big, "integer64")
  purrr::walk(rows[c("text", "integer", "flag", "day", "stamp", "big")], function(column) {
    testthat::expect_true(all(is.na(column)))
  })
  logical_columns <- c(
    "parenthesized", "coalesced", "logical_conditional", "cases", "nested_coalesce",
    "nested_conditional", "matched_flag", "matched_nested"
  )
  purrr::walk(rows[logical_columns], function(column) {
    testthat::expect_type(column, "logical")
    testthat::expect_true(all(is.na(column)))
  })
  testthat::expect_identical(rows$nan_missing, rep(NA_real_, 2L))
  testthat::expect_identical(rows$matched_number, c(NA_integer_, 3L))
  testthat::expect_identical(rows$inferred, rep(NA_integer_, 2L))
  testthat::expect_identical(rows$conditional, c(NA_integer_, 3L))
  full <- arrow::as_arrow_table(DBI::dbGetQueryArrow(con, as.character(dbplyr::remote_query(query))))
  empty <- query |>
    dplyr::filter(id < 0) |>
    dplyr::collect()
  empty_arrow <- arrow::as_arrow_table(DBI::dbGetQueryArrow(
    con,
    as.character(dbplyr::remote_query(query |> dplyr::filter(id < 0)))
  ))
  testthat::expect_identical(nrow(empty), 0L)
  testthat::expect_equal(empty, rows[0, ], ignore_attr = "tzone")
  testthat::expect_true(full$schema$Equals(empty_arrow$schema))
  cat("PASS typed missing dbplyr projections, contextual NA and matching empty schemas\n")
}

check_dbi_read_paths <- function(con) {
  quoted <- DBI::dbQuoteIdentifier(con, DBI::Id(schema = deployment$database, table = "types"))
  testthat::expect_true(DBI::dbExistsTable(con, quoted))
  testthat::expect_gt(length(DBI::dbListFields(con, quoted)), 0L)
  objects <- DBI::dbListObjects(con, prefix = DBI::Id(schema = deployment$database))
  testthat::expect_true(purrr::some(objects$table, function(object) identical(object@name[["table"]], "types")))
  prefix <- DBI::Id(catalog = "AwsDataCatalog", schema = deployment$database)
  testthat::expect_identical(DBI::dbListObjects(con, prefix), DBI::dbListObjects(con, DBI::dbQuoteIdentifier(con, prefix)))
  frame <- DBI::dbReadTable(con, quoted, row.names = NULL, check.names = FALSE)
  table <- arrow::as_arrow_table(DBI::dbReadTableArrow(con, quoted))
  testthat::expect_identical(nrow(frame), 2L)
  testthat::expect_equal(table$num_rows, 2, tolerance = 0)
  testthat::expect_s3_class(frame$big, "integer64")
  result <- DBI::dbSendQueryArrow(con, "SELECT id, big, arr FROM types")
  withr::defer(if (DBI::dbIsValid(result)) DBI::dbClearResult(result))
  testthat::expect_equal(DBI::dbGetInfo(result)$row.count, 0, tolerance = 0)
  chunk <- DBI::dbFetchArrowChunk(result)
  empty <- DBI::dbFetchArrowChunk(result)
  testthat::expect_equal(chunk$length, 2, tolerance = 0)
  testthat::expect_equal(empty$length, 0, tolerance = 0)
  testthat::expect_true(arrow::as_arrow_array(chunk)$type$Equals(arrow::as_arrow_array(empty)$type))
  testthat::expect_equal(DBI::dbGetInfo(result)$row.count, 2, tolerance = 0)
  testthat::expect_true(DBI::dbHasCompleted(result))
  DBI::dbClearResult(result)
  result <- DBI::dbSendQueryArrow(con, paste(
    "SELECT value, decimal_value, uint_value, whole_value FROM (VALUES",
    "(CAST('-9223372036854775808' AS BIGINT),",
    "CAST('12345678901234567890123456789.123456789' AS DECIMAL(38, 9)),",
    "CAST('18446744073709551615' AS DECIMAL(20, 0)),",
    "CAST('12345678901234567890123456789012345678' AS DECIMAL(38, 0))),",
    "(CAST('9007199254740993' AS BIGINT), CAST('0.000000001' AS DECIMAL(38, 9)),",
    "CAST('1' AS DECIMAL(20, 0)), CAST('1' AS DECIMAL(38, 0))),",
    "(CAST(NULL AS BIGINT), CAST(NULL AS DECIMAL(38, 9)),",
    "CAST(NULL AS DECIMAL(20, 0)), CAST(NULL AS DECIMAL(38, 0))))",
    "AS synthetic(value, decimal_value, uint_value, whole_value)",
    "WHERE /**/ value IS NOT DISTINCT FROM --\n?",
    "AND decimal_value IS NOT DISTINCT FROM ?",
    "AND uint_value IS NOT DISTINCT FROM ?",
    "AND whole_value IS NOT DISTINCT FROM ?"
  ))
  expected <- list(
    value = c("-9223372036854775808", "9007199254740993", NA_character_),
    decimal_value = c("12345678901234567890123456789.123456789", "0.000000001", NA_character_),
    uint_value = c("18446744073709551615", "1", NA_character_),
    whole_value = c("12345678901234567890123456789012345678", "1", NA_character_)
  )
  params <- arrow::Table$create(
    value = arrow::Array$create(expected$value)$cast(arrow::int64()),
    decimal_value = arrow::Array$create(expected$decimal_value)$cast(arrow::decimal128(38, 9)),
    uint_value = arrow::Array$create(expected$uint_value)$cast(arrow::uint64()),
    whole_value = arrow::Array$create(expected$whole_value)$cast(arrow::decimal128(38, 0))
  )
  DBI::dbBindArrow(result, nanoarrow::as_nanoarrow_array_stream(params))
  bound <- arrow::as_arrow_table(DBI::dbFetchArrow(result))
  for (name in names(expected)) {
    column <- params[[name]]
    if (name == "uint_value") column <- column$cast(arrow::decimal128(20, 0))
    testthat::expect_true(bound[[name]]$Equals(column))
  }
  DBI::dbClearResult(result)
  cat("PASS quoted catalog paths, schema prefixes, standard table reads, Arrow chunks/info and parameter streams\n")
}

check_managed_policy <- function(con) {
  changes <- tibble::tibble(table = "types", column = "s_null", new_table = "types", new_column = "s_empty", removed_on = "2099-01-01")
  testthat::local_mocked_bindings(column_changes = changes, .package = "ckdata")
  managed <- lake_tbl("types")
  direct <- dplyr::tbl(con, DBI::Id(schema = deployment$database, table = "types"))
  withr::with_options(list(ckdata.mode = "audit"), {
    testthat::expect_length(testthat::capture_warnings(dplyr::collect(managed)), 1L)
  })
  withr::with_options(list(ckdata.mode = "publish"), {
    testthat::expect_warning(published <- dplyr::collect(managed), class = "ckdata_flagged_columns")
    testthat::expect_no_warning(explicit <- dplyr::collect(direct))
    testthat::expect_false(rlang::has_name(published, "s_null"))
    testthat::expect_true(rlang::has_name(explicit, "s_null"))
  })
  cat("PASS managed policy runs once and leaves explicit DBI tables unchanged\n")
}

check_temporal_parameters <- function(con) {
  clocks <- c(secs = "00:00:01", mins = "00:01:00", hours = "01:00:00")
  for (units in names(clocks)) {
    value <- as.difftime(c(1, NA), units = units)
    sql <- paste0("SELECT 1 AS matched WHERE ? = TIME '", clocks[[units]], "'")
    testthat::expect_identical(DBI::dbGetQuery(con, sql, params = list(value))$matched, 1L)
  }
  testthat::expect_identical(DBI::dbGetQuery(con,
    "SELECT 1 AS matched WHERE ? = TIME '00:00:01.123456'",
    params = list(as.difftime(1.123456, units = "secs"))
  )$matched, 1L)
  testthat::expect_identical(DBI::dbGetQuery(con, "SELECT 1 AS matched WHERE ? IS NULL",
    params = list(as.difftime(NA_real_, units = "hours"))
  )$matched, 1L)
  testthat::expect_warning(result <- DBI::dbGetQuery(con, "SELECT ? AS value", params = list(factor("invented"))), "factor")
  testthat::expect_identical(result$value, "invented")
  cat("PASS difftime units/fractions/NULL predicates and factor-conversion warning\n")
}

check_dbi_literals <- function(con) {
  values <- list(
    flag = TRUE, text = "O'Brien", integer_value = 42L, number = 1.25,
    big = bit64::as.integer64("9007199254740993"), day = as.Date("1960-01-02"),
    stamp = as.POSIXct("2026-11-04 12:34:56.123", tz = "UTC"), bytes = list(as.raw(c(0, 255))),
    positive = Inf, negative = -Inf, nan_value = NaN, missing_text = NA_character_,
    missing_flag = NA, missing_number = NA_real_
  )
  expressions <- purrr::map_chr(values, function(x) {
    paste0(
      "CAST(", DBI::dbQuoteLiteral(con, x),
      " AS ", DBI::dbDataType(con, x), ")"
    )
  })
  sql <- paste("SELECT", paste(expressions, "AS", DBI::dbQuoteIdentifier(con, names(values)), collapse = ", "))
  actual <- DBI::dbGetQuery(con, sql)
  testthat::expect_identical(actual$flag, TRUE)
  testthat::expect_identical(actual$text, values$text)
  testthat::expect_identical(actual$integer_value, 42L)
  testthat::expect_identical(actual$number, 1.25)
  testthat::expect_identical(as.character(actual$big), "9007199254740993")
  testthat::expect_identical(actual$day, values$day)
  expect_timestamp_equal(actual$stamp, values$stamp)
  testthat::expect_identical(actual$bytes[[1]], values$bytes[[1]])
  testthat::expect_identical(actual$positive, Inf)
  testthat::expect_identical(actual$negative, -Inf)
  testthat::expect_true(is.nan(actual$nan_value))
  testthat::expect_type(actual$missing_text, "character")
  testthat::expect_true(is.na(actual$missing_text))
  testthat::expect_type(actual$missing_flag, "logical")
  testthat::expect_true(is.na(actual$missing_flag))
  testthat::expect_type(actual$missing_number, "double")
  testthat::expect_true(is.na(actual$missing_number))
  bool_sql <- DBI::sqlInterpolate(con,
    "SELECT flag FROM (VALUES (TRUE), (FALSE)) t(flag) WHERE flag = ?value",
    value = TRUE
  )
  inf_sql <- DBI::sqlInterpolate(con,
    "SELECT value FROM (VALUES (DOUBLE 'Infinity')) t(value) WHERE value = ?value",
    value = Inf
  )
  testthat::expect_identical(DBI::dbGetQuery(con, bool_sql)$flag, TRUE)
  testthat::expect_identical(DBI::dbGetQuery(con, inf_sql)$value, Inf)
  cat("PASS Athena DBI type names, quoted literals and SQL interpolation\n")
}

check_parameters <- function(con) {
  sql <- "SELECT ? AS text_value, ? AS empty_value, ? AS flag, ? AS integer_value, ? AS number, ? AS big, ? AS day, ? AS stamp, ? AS bytes"
  values <- list(
    "O'Brien ? --", "", TRUE, 42L, 1.25,
    bit64::as.integer64("9007199254740993"), as.Date("1960-01-02"),
    as.POSIXct("2026-11-03 12:34:56.123", tz = "UTC"), list(as.raw(c(0, 255)))
  )
  result <- DBI::dbSendQuery(con, sql)
  withr::defer(if (DBI::dbIsValid(result)) DBI::dbClearResult(result))
  testthat::expect_true(DBI::dbIsValid(result))
  testthat::expect_false(DBI::dbHasCompleted(result))
  testthat::expect_equal(DBI::dbGetRowCount(result), 0, tolerance = 0)
  DBI::dbBind(result, values)
  first <- DBI::dbFetch(result)
  testthat::expect_identical(first$text_value, values[[1]])
  testthat::expect_identical(first$empty_value, "")
  testthat::expect_identical(first$flag, TRUE)
  testthat::expect_identical(first$integer_value, 42L)
  testthat::expect_identical(first$number, 1.25)
  testthat::expect_identical(as.character(first$big), "9007199254740993")
  testthat::expect_identical(first$day, values[[7]])
  expect_timestamp_equal(first$stamp, values[[8]])
  testthat::expect_identical(first$bytes[[1]], values[[9]][[1]])
  nulls <- purrr::map(values, function(x) x[NA_integer_])
  DBI::dbBind(result, nulls)
  missing <- DBI::dbFetch(result)
  testthat::expect_identical(nrow(missing), 1L)
  purrr::walk(missing[1:8], function(column) testthat::expect_true(all(is.na(column))))
  testthat::expect_null(missing$bytes[[1]])
  DBI::dbBind(result, purrr::map(values, function(x) x[0]))
  empty <- DBI::dbFetch(result)
  testthat::expect_identical(nrow(empty), 0L)
  testthat::expect_equal(empty, first[0, ], ignore_attr = "tzone")
  DBI::dbClearResult(result)
  result <- DBI::dbSendQuery(con, "SELECT id, big, arr FROM types WHERE id = ? /* ? */ -- ?\n")
  DBI::dbBind(result, list(c(2L, 1L)))
  testthat::expect_identical(DBI::dbFetch(result, 1)$id, 2L)
  testthat::expect_false(DBI::dbHasCompleted(result))
  testthat::expect_identical(DBI::dbFetch(result)$id, 1L)
  testthat::expect_equal(DBI::dbGetRowCount(result), 2, tolerance = 0)
  DBI::dbBind(result, list(999L))
  empty <- DBI::dbFetch(result)
  testthat::expect_identical(nrow(empty), 0L)
  testthat::expect_s3_class(empty$big, "integer64")
  testthat::expect_type(empty$arr, "list")
  DBI::dbBind(result, list(1L))
  testthat::expect_identical(DBI::dbFetch(result)$id, 1L)
  DBI::dbClearResult(result)
  sql <- "SELECT ? AS big, ? AS number"
  full <- arrow::as_arrow_table(DBI::dbGetQueryArrow(con, sql, params = list(bit64::as.integer64(1), 1)))
  empty <- arrow::as_arrow_table(DBI::dbGetQueryArrow(con, sql, params = list(bit64::integer64(), numeric())))
  testthat::expect_true(full$schema$Equals(empty$schema))
  testthat::expect_identical(full$big$type$ToString(), "int64")
  testthat::expect_identical(full$number$type$ToString(), "double")
  specials <- DBI::dbGetQuery(con, "SELECT ? AS positive, ? AS negative, ? AS nan_value",
    params = list(Inf, -Inf, NaN)
  )
  testthat::expect_identical(specials$positive, Inf)
  testthat::expect_identical(specials$negative, -Inf)
  testthat::expect_true(is.nan(specials$nan_value))
  result <- DBI::dbSendQuery(con, "SELECT CAST(? AS INTEGER) AS value", params = list("3"))
  testthat::expect_error(DBI::dbBind(result, list(c("1", "invented-invalid-integer"))), "INVALID_CAST_ARGUMENT", class = "ckdata_native_error")
  testthat::expect_identical(DBI::dbFetch(result)$value, 3L)
  DBI::dbBind(result, list("4"))
  testthat::expect_identical(DBI::dbFetch(result)$value, 4L)
  DBI::dbClearResult(result)
  cat("PASS native query binding, scalar types/NULLs, vectorized/repeated/zero-length binding, Arrow and empty recovery\n")
}

check_saved_results <- function(con) {
  namespace <- rlang::ns_env("ckdata")
  native_query <- rlang::env_get(namespace, "native_query")
  calls <- 0L
  # Count submissions while retaining the real AWS/Arrow path.
  testthat::local_mocked_bindings(native_query = function(...) {
    calls <<- calls + 1L
    native_query(...)
  }, .package = "ckdata")
  withr::local_options(ckdata.freshness = daily_at("00:00", "UTC"))
  sql <- "SELECT BIGINT '9007199254740993' AS big, BIGINT '-9223372036854775808' AS minimum, CAST('12345678901234567890.123456789' AS DECIMAL(38,9)) AS wide, ARRAY[CAST('1.10' AS DECIMAL(20,2)), CAST(NULL AS DECIMAL(20,2))] AS nested"
  columns <- c("big", "minimum", "wide", "nested")
  query <- dplyr::tbl(con, dbplyr::sql(sql), vars = columns)
  directory <- withr::local_tempdir(pattern = "ckdata-synthetic-saved-")
  first <- collect_saved(query, dir = directory)
  testthat::expect_identical(collect_saved(query, dir = directory), first)
  testthat::expect_identical(calls, 1L)
  testthat::expect_identical(collect_saved(query, refresh = TRUE, dir = directory), first)
  testthat::expect_identical(calls, 2L)
  snapshot <- collect_saved(query, snapshot = "baseline", dir = directory)
  testthat::expect_identical(collect_saved(query, snapshot = "baseline", dir = directory), snapshot)
  testthat::expect_identical(calls, 3L)
  file <- list.files(directory, pattern = "^snapshot-baseline-", full.names = TRUE)
  parquet <- arrow::read_parquet(file, as_data_frame = FALSE)
  expected <- list(
    big = arrow::Array$create("9007199254740993")$cast(arrow::int64()),
    minimum = arrow::Array$create("-9223372036854775808")$cast(arrow::int64()),
    wide = arrow::Array$create("12345678901234567890.123456789")$cast(arrow::decimal128(38, 9))
  )
  purrr::iwalk(expected, function(value, name) {
    testthat::expect_true(parquet[[name]]$Equals(arrow::ChunkedArray$create(value)))
  })
  testthat::expect_identical(parquet$nested$type$value_type$ToString(), "decimal128(20, 2)")
  empty <- dplyr::tbl(con, dbplyr::sql(paste0("SELECT * FROM (", sql, ") invented WHERE FALSE")), vars = columns)
  testthat::expect_identical(nrow(collect_saved(empty, snapshot = "empty", dir = directory)), 0L)
  empty_file <- list.files(directory, pattern = "^snapshot-empty-", full.names = TRUE)
  empty_parquet <- arrow::read_parquet(empty_file, as_data_frame = FALSE)
  testthat::expect_true(parquet$schema$Equals(empty_parquet$schema, check_metadata = FALSE))
  cat("PASS freshness reuse without submission, refresh, frozen snapshots and exact/nested/empty Parquet saves\n")
}

main <- function() {
  testthat::local_edition(3)
  withr::local_options(list(
    ckdata.workgroup = seat$workgroup, ckdata.schema = deployment$database,
    ckdata.mode = "explore", ckdata.timeout = 120
  ))
  withr::local_envvar(c(
    CK_DATALAKE_INTERNAL_PROFILE = NA_character_, CK_DATALAKE_REGION = NA_character_,
    CK_DATALAKE_WORKGROUP = seat$workgroup, CK_DATALAKE_SCHEMA = deployment$database,
    CK_DATALAKE_ROLE_ARN = seat$role_arn, AWS_REGION = deployment$region
  ))
  con <- DBI::dbConnect(ckdata::ckdata(),
    workgroup = seat$workgroup,
    schema = deployment$database, region = deployment$region, role_arn = seat$role_arn
  )
  managed <- NULL
  close <- function(connection) {
    if (!rlang::is_null(connection)) {
      suppressWarnings(rlang::try_fetch(DBI::dbDisconnect(connection), error = function(cnd) NULL))
    }
  }
  withr::defer({
    close(managed)
    close(con)
  })
  fence(con)
  cat("PASS", seat_name, ": synthetic identity, workgroup and schema\n")
  # This must precede Arrow retrieval: exercise ordinary DBI in a fresh session.
  testthat::expect_identical(nrow(DBI::dbGetQuery(con, probe)), 2L)
  cat("PASS first-query DBI conversion in a fresh R session\n")
  for (sql in list(scalar_probe, nested_probe)) {
    baseline <- lake_query(sql, as = "arrow", con = con)
    frame <- DBI::dbGetQuery(con, sql)
    testthat::expect_equal(baseline$num_rows, 1, tolerance = 0)
    testthat::expect_identical(nrow(frame), 1L)
    if (identical(sql, scalar_probe)) check_scalar_values(baseline, frame) else check_nested_values(baseline, frame)
    for (empty_sql in c(
      paste0("SELECT * FROM (", sql, ") q WHERE FALSE"),
      paste0(sql, " LIMIT 0"),
      paste0("WITH invented AS (", sql, ") SELECT * FROM invented OFFSET 1")
    )) {
      empty <- lake_query(empty_sql, as = "arrow", con = con)
      testthat::expect_equal(empty$num_rows, 0, tolerance = 0)
      testthat::expect_true(baseline$schema$Equals(empty$schema, check_metadata = TRUE))
      testthat::expect_equal(DBI::dbGetQuery(con, empty_sql), frame[0, ], ignore_attr = "tzone")
    }
  }
  cat("PASS exact scalar/nested values, wide Arrow decimals and matching typed empty R results\n")
  result <- DBI::dbSendQuery(con, probe)
  testthat::expect_identical(nrow(DBI::dbFetch(result, 0)), 0L)
  testthat::expect_equal(DBI::dbGetRowCount(result), 0, tolerance = 0)
  first <- DBI::dbFetch(result, 1)
  second <- DBI::dbFetch(result, Inf)
  testthat::expect_identical(nrow(first), 1L)
  testthat::expect_identical(nrow(second), 1L)
  testthat::expect_true(DBI::dbHasCompleted(result))
  testthat::expect_equal(DBI::dbGetRowCount(result), 2, tolerance = 0)
  DBI::dbClearResult(result)
  combined <- dplyr::bind_rows(first, second)
  combined <- combined[order(combined$id), ]
  testthat::expect_true(is.na(combined$s_null[1]))
  testthat::expect_identical(combined$s_empty[1], "")
  testthat::expect_identical(as.character(combined$big[1]), "9007199254740993")
  null <- DBI::dbGetQuery(con, "SELECT CAST(NULL AS BIGINT) AS all_null")
  testthat::expect_identical(nrow(null), 1L)
  testthat::expect_true(is.na(null$all_null))
  cat("PASS DBI paging, NULL/empty-string/int64 values and all-NULL real row\n")

  managed <- rlang::env_get(rlang::ns_env("ckdata"), "lake_con")()
  fence(managed)
  tbl <- lake_tbl("types")
  query <- tbl |>
    dplyr::filter(id > 0) |>
    dplyr::select(id, big)
  count <- tbl |>
    dplyr::summarise(n = dplyr::n()) |>
    dplyr::collect()
  testthat::expect_identical(nrow(count), 1L)
  testthat::expect_equal(as.numeric(count$n), 2, tolerance = 0)
  joined <- dplyr::inner_join(query, query, by = "id") |> dplyr::collect()
  testthat::expect_identical(nrow(joined), 2L)
  cat("PASS named-table metadata, dbplyr filtering/join/summarise\n")
  check_saved_results(managed)

  check_inline_queries(con)
  check_typed_missing(con)
  check_dbi_literals(con)
  check_dbi_read_paths(con)
  check_managed_policy(con)
  check_temporal_parameters(con)
  check_parameters(con)
  check_temporal_range(con)
  check_nullable_scalars(con)
  check_binary_values(con)

  literal <- dplyr::tbl(con, dbplyr::sql("SELECT * FROM (VALUES (1, DOUBLE 'Infinity', DATE '2026-11-03'), (2, DOUBLE '-Infinity', DATE '2026-12-04'), (3, CAST(NULL AS DOUBLE), DATE '2026-01-01'), (4, DOUBLE 'NaN', DATE '2026-01-02'), (5, DOUBLE '1.25', DATE '2026-01-03')) t(id, value, stamp)"))
  translated <- literal |>
    dplyr::mutate(infinite = is.infinite(value), finite = is.finite(value), nan = is.nan(value), month_value = month(stamp)) |>
    dplyr::collect() |>
    dplyr::arrange(id)
  testthat::expect_identical(translated$infinite, c(TRUE, TRUE, FALSE, FALSE, FALSE))
  testthat::expect_identical(translated$finite, c(FALSE, FALSE, FALSE, FALSE, TRUE))
  testthat::expect_identical(translated$nan, c(FALSE, FALSE, FALSE, TRUE, FALSE))
  testthat::expect_identical(as.integer(sort(translated$month_value)), c(1L, 1L, 1L, 11L, 12L))
  approximate <- tbl |>
    dplyr::summarise(
      m = approx_median(id, na.rm = TRUE),
      q = approx_quantile(id, 0.95, na.rm = TRUE)
    ) |>
    dplyr::collect()
  # Two fixture IDs only check execution and output range, not quantile accuracy.
  testthat::expect_identical(nrow(approximate), 1L)
  testthat::expect_gte(approximate$m, 1)
  testthat::expect_lte(approximate$m, 2)
  testthat::expect_gte(approximate$q, 1)
  testthat::expect_lte(approximate$q, 2)
  functions <- dplyr::tbl(con, dbplyr::sql("SELECT * FROM (VALUES (1, 'Alpha', TIMESTAMP '2026-11-01 12:34:56.789'), (2, 'a.b', TIMESTAMP '2026-11-02 00:00:00.001'), (3, CAST(NULL AS VARCHAR), CAST(NULL AS TIMESTAMP))) t(id, text_value, stamp)")) |>
    dplyr::mutate(
      regex = grepl("^a", text_value), insensitive = grepl("^a", text_value, ignore.case = TRUE),
      fixed = grepl("a.b", text_value, fixed = TRUE),
      missing_pattern = grepl(NA_character_, text_value),
      month_floor = floor_date(stamp, "month"), second_floor = floor_date(stamp),
      sunday_floor = floor_date(stamp, "week"), monday_floor = floor_date(stamp, "week", week_start = 1)
    ) |>
    dplyr::collect() |>
    dplyr::arrange(id)
  testthat::expect_identical(functions$regex, c(FALSE, TRUE, FALSE))
  testthat::expect_identical(functions$insensitive, c(TRUE, TRUE, FALSE))
  testthat::expect_identical(functions$fixed, c(FALSE, TRUE, FALSE))
  testthat::expect_type(functions$missing_pattern, "logical")
  testthat::expect_true(all(is.na(functions$missing_pattern)))
  expect_timestamp_equal(functions$month_floor, as.POSIXct(c("2026-11-01 00:00:00", "2026-11-01 00:00:00", NA), tz = "UTC"), tolerance = 0)
  expect_timestamp_equal(functions$second_floor, as.POSIXct(c("2026-11-01 12:34:56", "2026-11-02 00:00:00", NA), tz = "UTC"), tolerance = 0)
  expect_timestamp_equal(functions$sunday_floor, as.POSIXct(c("2026-11-01 00:00:00", "2026-11-01 00:00:00", NA), tz = "UTC"), tolerance = 0)
  expect_timestamp_equal(functions$monday_floor, as.POSIXct(c("2026-10-26 00:00:00", "2026-11-02 00:00:00", NA), tz = "UTC"), tolerance = 0)
  cat("PASS finite/NaN/infinite helpers, regex, date truncation/week starts and approximate summary execution/range smoke checks\n")
  # Standard DBItest round-trip templates use unnamed expressions and an
  # untyped NULL output. Athena UNLOAD rejects both. Check those boundaries,
  # then exercise quoting with named, typed output; no production SQL rewrite.
  testthat::expect_error(DBI::dbGetQuery(con, "SELECT 1"), "MISSING_COLUMN_NAME")
  testthat::expect_error(DBI::dbGetQuery(con, "SELECT NULL AS value"), "COLUMN_TYPE_UNKNOWN")
  characters <- c("", " ", "\t", "'", '"', "`", "\n")
  combinations <- expand.grid(characters, "a", characters, "b", characters,
    stringsAsFactors = FALSE
  )
  strings <- do.call(paste0, combinations)
  strings <- c(strings, "naïve 🧪 漢字", "back\\'quote", "\r", "N/A")
  quoted <- as.character(DBI::dbQuoteString(con, strings))
  strings <- c(strings, quoted, as.character(DBI::dbQuoteString(con, quoted)))
  values <- paste0("(", DBI::dbQuoteString(con, strings), ")", collapse = ",")
  roundtrip <- DBI::dbGetQuery(con, paste0("SELECT value FROM (VALUES ", values, ") AS t(value)"))
  testthat::expect_identical(sort(roundtrip$value, method = "radix"), sort(strings, method = "radix"))
  literals <- list(
    1L, 2.5, "string", TRUE,
    as.Date("2026-11-03"), as.POSIXct("2026-11-03 12:34:56.123", tz = "UTC")
  )
  expressions <- purrr::map_chr(literals, function(x) as.character(DBI::dbQuoteLiteral(con, x)))
  expressions <- paste(expressions, "AS", paste0("value", seq_along(expressions)))
  roundtrip <- DBI::dbGetQuery(con, paste("SELECT", paste(expressions, collapse = ",")))
  for (column in which(purrr::map_lgl(literals, is.numeric))) {
    roundtrip[[column]] <- as.numeric(roundtrip[[column]])
  }
  # UNLOAD timestamps are naive: compare instants without a timezone annotation.
  for (column in seq_along(literals)) {
    if (inherits(literals[[column]], "POSIXct")) {
      expect_timestamp_equal(roundtrip[[column]], literals[[column]])
    } else {
      testthat::expect_equal(roundtrip[[column]], literals[[column]], ignore_attr = TRUE)
    }
  }
  null <- DBI::dbQuoteString(con, NA_character_)
  roundtrip <- DBI::dbGetQuery(con, paste0(
    "SELECT CAST(", null, " AS VARCHAR) AS null_value, ",
    DBI::dbQuoteString(con, "NULL"), " AS text_value, ",
    DBI::dbQuoteString(con, as.character(null)), " AS quoted_value"
  ))
  testthat::expect_true(is.na(roundtrip$null_value))
  testthat::expect_identical(roundtrip$text_value, "NULL")
  testthat::expect_identical(roundtrip$quoted_value, as.character(null))
  cat("PASS DBI literal round trips with named/typed output; UNLOAD limits confirmed\n")
  DBI::dbDisconnect(managed)
  DBI::dbDisconnect(con)
  managed <- NULL
  con <- NULL
  cat("PASS", seat_name, ": all R acceptance checks; no real data queried\n")
}
main()
