test_that("Athena catalog paths, table reads and Arrow binding use the DBI interface", {
  context <- local_live_context()
  con <- context$con
  deployment <- context$deployment
  quoted <- DBI::dbQuoteIdentifier(
    con,
    DBI::Id(schema = deployment$database, table = "types")
  )
  testthat::expect_true(DBI::dbExistsTable(con, quoted))
  testthat::expect_gt(length(DBI::dbListFields(con, quoted)), 0L)
  objects <- DBI::dbListObjects(
    con,
    prefix = DBI::Id(schema = deployment$database)
  )
  testthat::expect_true(purrr::some(objects$table, function(object) {
    identical(object@name[["table"]], "types")
  }))
  prefix <- DBI::Id(catalog = "AwsDataCatalog", schema = deployment$database)
  testthat::expect_identical(
    DBI::dbListObjects(con, prefix),
    DBI::dbListObjects(con, DBI::dbQuoteIdentifier(con, prefix))
  )
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
  testthat::expect_true(arrow::as_arrow_array(chunk)$type$Equals(
    arrow::as_arrow_array(empty)$type
  ))
  testthat::expect_equal(DBI::dbGetInfo(result)$row.count, 2, tolerance = 0)
  testthat::expect_true(DBI::dbHasCompleted(result))
  DBI::dbClearResult(result)
  result <- DBI::dbSendQueryArrow(
    con,
    paste(
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
    )
  )
  expected <- list(
    value = c("-9223372036854775808", "9007199254740993", NA_character_),
    decimal_value = c(
      "12345678901234567890123456789.123456789",
      "0.000000001",
      NA_character_
    ),
    uint_value = c("18446744073709551615", "1", NA_character_),
    whole_value = c(
      "12345678901234567890123456789012345678",
      "1",
      NA_character_
    )
  )
  params <- arrow::Table$create(
    value = arrow::Array$create(expected$value)$cast(arrow::int64()),
    decimal_value = arrow::Array$create(
      expected$decimal_value
    )$cast(arrow::decimal128(38, 9)),
    uint_value = arrow::Array$create(expected$uint_value)$cast(arrow::uint64()),
    whole_value = arrow::Array$create(
      expected$whole_value
    )$cast(arrow::decimal128(38, 0))
  )
  DBI::dbBindArrow(result, nanoarrow::as_nanoarrow_array_stream(params))
  bound <- arrow::as_arrow_table(DBI::dbFetchArrow(result))
  for (name in names(expected)) {
    column <- params[[name]]
    if (name == "uint_value") {
      column <- column$cast(arrow::decimal128(20, 0))
    }
    testthat::expect_true(bound[[name]]$Equals(column))
  }
  DBI::dbClearResult(result)
})

test_that("DBI type names, quoting and interpolation execute correctly in Athena", {
  context <- local_live_context()
  con <- context$con
  values <- list(
    flag = TRUE,
    text = "O'Brien",
    integer_value = 42L,
    number = 1.25,
    big = bit64::as.integer64("9007199254740993"),
    day = as.Date("1960-01-02"),
    stamp = as.POSIXct("2026-11-04 12:34:56.123", tz = "UTC"),
    bytes = list(as.raw(c(0, 255))),
    positive = Inf,
    negative = -Inf,
    nan_value = NaN,
    missing_text = NA_character_,
    missing_flag = NA,
    missing_number = NA_real_
  )
  expressions <- purrr::map_chr(values, function(x) {
    paste0(
      "CAST(",
      DBI::dbQuoteLiteral(con, x),
      " AS ",
      DBI::dbDataType(con, x),
      ")"
    )
  })
  sql <- paste(
    "SELECT",
    paste(
      expressions,
      "AS",
      DBI::dbQuoteIdentifier(con, names(values)),
      collapse = ", "
    )
  )
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
  bool_sql <- DBI::sqlInterpolate(
    con,
    "SELECT flag FROM (VALUES (TRUE), (FALSE)) t(flag) WHERE flag = ?value",
    value = TRUE
  )
  inf_sql <- DBI::sqlInterpolate(
    con,
    "SELECT value FROM (VALUES (DOUBLE 'Infinity')) t(value) WHERE value = ?value",
    value = Inf
  )
  testthat::expect_identical(DBI::dbGetQuery(con, bool_sql)$flag, TRUE)
  testthat::expect_identical(DBI::dbGetQuery(con, inf_sql)$value, Inf)
})

test_that("Athena DBI paging retains nulls, exact integers and actual all-null rows", {
  context <- local_live_context()
  con <- context$con
  result <- DBI::dbSendQuery(con, probe)
  withr::defer(if (DBI::dbIsValid(result)) DBI::dbClearResult(result))
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
})

test_that("Athena quoting round trips Unicode and documents UNLOAD output restrictions", {
  context <- local_live_context()
  con <- context$con
  # Standard DBItest round-trip templates use unnamed expressions and an
  # untyped NULL output. Athena UNLOAD rejects both. Check those boundaries,
  # then exercise quoting with named, typed output; no production SQL rewrite.
  testthat::expect_error(
    DBI::dbGetQuery(con, "SELECT 1"),
    "MISSING_COLUMN_NAME"
  )
  testthat::expect_error(
    DBI::dbGetQuery(con, "SELECT NULL AS value"),
    "COLUMN_TYPE_UNKNOWN"
  )
  characters <- c("", " ", "\t", "'", '"', "`", "\n")
  combinations <- expand.grid(
    characters,
    "a",
    characters,
    "b",
    characters,
    stringsAsFactors = FALSE
  )
  strings <- do.call(paste0, combinations)
  strings <- c(strings, "naïve 🧪 漢字", "back\\'quote", "\r", "N/A")
  quoted <- as.character(DBI::dbQuoteString(con, strings))
  strings <- c(strings, quoted, as.character(DBI::dbQuoteString(con, quoted)))
  values <- paste0("(", DBI::dbQuoteString(con, strings), ")", collapse = ",")
  roundtrip <- DBI::dbGetQuery(
    con,
    paste0("SELECT value FROM (VALUES ", values, ") AS t(value)")
  )
  testthat::expect_identical(
    sort(roundtrip$value, method = "radix"),
    sort(strings, method = "radix")
  )
  literals <- list(
    1L,
    2.5,
    "string",
    TRUE,
    as.Date("2026-11-03"),
    as.POSIXct("2026-11-03 12:34:56.123", tz = "UTC")
  )
  expressions <- purrr::map_chr(literals, function(x) {
    as.character(DBI::dbQuoteLiteral(con, x))
  })
  expressions <- paste(
    expressions,
    "AS",
    paste0("value", seq_along(expressions))
  )
  roundtrip <- DBI::dbGetQuery(
    con,
    paste("SELECT", paste(expressions, collapse = ","))
  )
  for (column in which(purrr::map_lgl(literals, is.numeric))) {
    roundtrip[[column]] <- as.numeric(roundtrip[[column]])
  }
  # UNLOAD timestamps are naive: compare instants without a timezone annotation.
  for (column in seq_along(literals)) {
    if (inherits(literals[[column]], "POSIXct")) {
      expect_timestamp_equal(roundtrip[[column]], literals[[column]])
    } else {
      testthat::expect_equal(
        roundtrip[[column]],
        literals[[column]],
        ignore_attr = TRUE
      )
    }
  }
  null <- DBI::dbQuoteString(con, NA_character_)
  roundtrip <- DBI::dbGetQuery(
    con,
    paste0(
      "SELECT CAST(",
      null,
      " AS VARCHAR) AS null_value, ",
      DBI::dbQuoteString(con, "NULL"),
      " AS text_value, ",
      DBI::dbQuoteString(con, as.character(null)),
      " AS quoted_value"
    )
  )
  testthat::expect_true(is.na(roundtrip$null_value))
  testthat::expect_identical(roundtrip$text_value, "NULL")
  testthat::expect_identical(roundtrip$quoted_value, as.character(null))
})
