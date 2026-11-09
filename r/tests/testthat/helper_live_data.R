# Invented SQL values for the opt-in Athena tests. No query runs on sourcing.
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
  expected <- arrow::Array$create(
    values,
    type = arrow::utf8()
  )$cast(arrow::decimal128(38, 18))
  if (inherits(column, "ChunkedArray")) {
    expected <- arrow::ChunkedArray$create(expected)
  }
  column$Equals(expected)
}
arrow_micros <- function(column) {
  withr::with_options(
    list(arrow.int64_downcast = FALSE),
    as.character(as.vector(column$cast(arrow::timestamp(
      "us"
    ))$cast(arrow::int64())))
  )
}

expect_live_scalar_values <- function(table, frame) {
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
  testthat::expect_true(arrow_decimal_matches(
    table$decimal_wide,
    "12345678901234567890.123456789012345678"
  ))
  testthat::expect_true(arrow_decimal_matches(
    table$decimal_negative,
    "-12345678901234567890.123456789012345678"
  ))
  testthat::expect_true(arrow_decimal_matches(table$decimal_zero, "0"))
  testthat::expect_true(arrow_decimal_matches(
    table$decimal_null,
    NA_character_
  ))
  testthat::expect_identical(arrow_micros(table$moment), "1792154096789000")
  testthat::expect_identical(
    arrow_micros(table$before_epoch),
    "-315521754322000"
  )
}

expect_live_nested_values <- function(table, frame) {
  nullable <- table$array_nullable$chunk(0)$values()
  record <- table$named_record$chunk(0)
  records <- table$record_array$chunk(0)$values()
  mapping <- table$map_decimal$chunk(0)
  map_records <- frame$map_records[[1]]
  testthat::expect_identical(
    as.character(frame$array_nullable[[1]]),
    c("9007199254740993", NA, "-4")
  )
  testthat::expect_identical(nullable$type$ToString(), "int64")
  testthat::expect_length(frame$empty_array[[1]], 0L)
  testthat::expect_true(arrow_decimal_matches(
    table$decimal_array$chunk(0)$values(),
    c("1.123456789012345678", NA)
  ))
  testthat::expect_identical(as.vector(mapping$keys()), c("a", "b"))
  testthat::expect_true(arrow_decimal_matches(mapping$items(), c("1.10", NA)))
  testthat::expect_identical(
    as.character(frame$named_record$id),
    "9007199254740993"
  )
  testthat::expect_identical(frame$named_record$label, "invented")
  testthat::expect_identical(frame$named_record$items[[1]], c(1L, NA, 2L))
  testthat::expect_identical(
    as.character(as.vector(record$GetFieldByName("id"))),
    "9007199254740993"
  )
  testthat::expect_false(records$IsNull(0))
  testthat::expect_true(records$IsNull(1))
  testthat::expect_identical(
    as.character(frame$record_array[[1]]$id),
    c("3", NA)
  )
  testthat::expect_identical(frame$record_array[[1]]$items[[1]], c(1L, NA))
  testthat::expect_identical(map_records$key, c("a", "b"))
  testthat::expect_null(map_records$value[[2]])
  testthat::expect_identical(map_records$value[[1]]$amount, 1.1)
  expect_timestamp_equal(map_records$value[[1]]$stamp, 1792154096.789)
  testthat::expect_identical(frame$deep_record$matrix[[1]][[1]], c(1L, NA))
  testthat::expect_null(frame$deep_record$matrix[[1]][[2]])
  testthat::expect_length(frame$deep_record$matrix[[1]][[3]], 0L)
  testthat::expect_identical(frame$deep_record$lookup[[1]]$key, "a")
  testthat::expect_identical(
    as.character(frame$deep_record$lookup[[1]]$value[[1]]),
    c("9007199254740993", NA)
  )
  testthat::expect_true(all(is.na(frame$null_record$id)))
  testthat::expect_null(frame$null_record$items[[1]])
}
