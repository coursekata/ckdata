test_that("Arrow conversion retains nested int64 and stable empty column types", {
  table <- arrow::Table$create(
    big = bit64::as.integer64(c("9007199254740993", NA)),
    decimal = arrow::Array$create(c(
      "12345678901234567890.1234",
      NA
    ))$cast(arrow::decimal128(38, 4)),
    values = arrow::Array$create(
      list(bit64::as.integer64("9007199254740993"), NULL),
      type = arrow::list_of(arrow::int64())
    ),
    record = arrow::Array$create(
      data.frame(id = bit64::as.integer64(c("9007199254740993", NA))),
      type = arrow::struct(id = arrow::int64())
    ),
    date = as.Date(c("2026-11-03", NA)),
    timestamp = as.POSIXct(c("2026-11-03 12:34:56", NA), tz = "UTC"),
    text = c("", NA_character_)
  )
  frame <- stream_frame(nanoarrow::as_nanoarrow_array_stream(table))
  empty <- stream_frame(nanoarrow::as_nanoarrow_array_stream(table$Slice(0, 0)))
  expect_equal(empty, frame[0, ])
  expect_identical(as.character(frame$big[1]), "9007199254740993")
  expect_identical(as.character(frame$values[[1]]), "9007199254740993")
  expect_identical(as.character(frame$record$id[1]), "9007199254740993")
  expect_type(frame$decimal, "double")
  expect_s3_class(frame$date, "Date")
  expect_s3_class(frame$timestamp, "POSIXct")
  expect_identical(attr(frame$timestamp, "tzone"), "UTC")
  expect_identical(frame$text, c("", NA_character_))
})

test_that("saved result refresh and context isolation use the actual DBI path", {
  local_saved_policy()
  fixture <- local_native_connection()
  tbl <- dplyr::tbl(fixture$con, "types")
  dir <- withr::local_tempdir()
  first <- collect_saved(tbl, dir = dir)
  cached <- collect_saved(tbl, dir = dir)
  expect_identical(cached, first)
  expect_length(fixture$calls$queries, 1L)
  refreshed <- collect_saved(tbl, dir = dir, refresh = TRUE)
  expect_identical(refreshed, first)
  expect_length(fixture$calls$queries, 2L)
  fixture$calls$account <- "111111111111"
  expect_identical(collect_saved(tbl, dir = dir), first)
  expect_length(fixture$calls$queries, 3L)
  expect_length(list.files(dir, pattern = "[.]parquet$"), 2L)
})

test_that("R conversion maps integer minima to its missing sentinels", {
  integer <- arrow::Array$create(c(
    "-2147483648",
    "2147483647",
    NA
  ))$cast(arrow::int32())
  big <- arrow::Array$create(c(
    "-9223372036854775808",
    "9223372036854775807",
    NA
  ))$cast(arrow::int64())
  frame <- table_frame(arrow::Table$create(integer = integer, big = big))
  expect_identical(frame$integer, c(NA_integer_, 2147483647L, NA_integer_))
  expect_identical(as.character(frame$big), c(NA, "9223372036854775807", NA))
})
