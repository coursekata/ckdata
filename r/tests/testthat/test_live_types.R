test_that("Athena preserves historical, future and nested temporal values", {
  context <- local_live_context()
  con <- context$con
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
  expected <- c(
    "-11675998554322000",
    "-1000",
    "10416657906789000",
    "253402300799999000",
    NA,
    "-62135596799999000"
  )
  frame <- DBI::dbGetQuery(con, sql)
  frame <- frame[order(frame$id), ]
  actual <- data.frame(
    id = as.vector(table$id),
    stamp = arrow_micros(table$stamp)
  )
  testthat::expect_identical(actual$stamp[order(actual$id)], expected)
  testthat::expect_identical(
    frame$day_value,
    as.Date(c(
      "1600-01-02",
      "1969-12-31",
      "2300-02-03",
      "9999-12-31",
      NA,
      "0001-01-01"
    ))
  )
  expected_epochs <- c(
    -11675998554.322,
    -0.001,
    10416657906.789,
    253402300799.999,
    NA,
    -62135596799.999
  )
  expect_timestamp_equal(frame$stamp, expected_epochs)
  for (i in which(!is.na(frame$stamp))) {
    testthat::expect_identical(
      as.numeric(frame$stamps[[i]][1]),
      as.numeric(frame$stamp[i])
    )
    testthat::expect_true(is.na(frame$stamps[[i]][2]))
    testthat::expect_identical(
      as.numeric(frame$record_value$stamp[i]),
      as.numeric(frame$stamp[i])
    )
    testthat::expect_identical(frame$mapping[[i]]$key, c("a", "b"))
    testthat::expect_identical(
      as.numeric(frame$mapping[[i]]$value[1]),
      as.numeric(frame$stamp[i])
    )
    testthat::expect_true(is.na(frame$mapping[[i]]$value[2]))
  }
  empty <- lake_query(
    paste0("SELECT * FROM (", sql, ") q WHERE FALSE"),
    as = "arrow",
    con = con
  )
  testthat::expect_equal(empty$num_rows, 0, tolerance = 0)
  testthat::expect_true(table$schema$Equals(
    empty$schema,
    check_metadata = TRUE
  ))
})

test_that("Athena nullable scalar widths survive partial DBI fetches", {
  context <- local_live_context()
  con <- context$con
  sql <- r"(SELECT id, CAST(tiny AS TINYINT) AS tiny, CAST(small AS SMALLINT) AS small,
  CAST(single_value AS REAL) AS single_value, flag, big
  FROM (VALUES (1, -128, -32768, -1.25, TRUE, BIGINT '9007199254740993'),
  (2, 127, 32767, 1.25, FALSE, BIGINT '9223372036854775807'),
  (3, NULL, NULL, NULL, NULL, NULL)) t(id, tiny, small, single_value, flag, big))"
  result <- DBI::dbSendQuery(con, sql)
  withr::defer(DBI::dbClearResult(result))
  info <- DBI::dbColumnInfo(result)
  rows <- list(
    DBI::dbFetch(result, 1),
    DBI::dbFetch(result, 1),
    DBI::dbFetch(result)
  )
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
  testthat::expect_identical(
    as.character(frame$big),
    c("9007199254740993", "9223372036854775807", NA)
  )
  testthat::expect_identical(DBI::dbColumnInfo(result), info)
  testthat::expect_true(DBI::dbHasCompleted(result))
})

test_that("Athena binding distinguishes missing, empty and arbitrary binary values", {
  context <- local_live_context()
  con <- context$con
  values <- list(NULL, raw(), as.raw(0:255))
  result <- DBI::dbSendQuery(con, "SELECT ? AS bytes", params = list(values))
  withr::defer(DBI::dbClearResult(result))
  pieces <- purrr::map(seq_along(values), function(i) {
    DBI::dbFetch(result, 1)$bytes[[1]]
  })
  testthat::expect_identical(pieces, values)
  testthat::expect_true(DBI::dbHasCompleted(result))
  table <- arrow::as_arrow_table(DBI::dbGetQueryArrow(
    con,
    "SELECT ? AS bytes",
    params = list(values)
  ))
  testthat::expect_identical(table$bytes$type$ToString(), "binary")
  testthat::expect_identical(
    purrr::map(as.vector(table$bytes), identity),
    values
  )
})

test_that("Athena scalar and nested empty results retain their populated schemas", {
  context <- local_live_context()
  con <- context$con
  testthat::expect_identical(nrow(DBI::dbGetQuery(con, probe)), 2L)
  for (sql in list(scalar_probe, nested_probe)) {
    baseline <- lake_query(sql, as = "arrow", con = con)
    frame <- DBI::dbGetQuery(con, sql)
    testthat::expect_equal(baseline$num_rows, 1, tolerance = 0)
    testthat::expect_identical(nrow(frame), 1L)
    if (identical(sql, scalar_probe)) {
      expect_live_scalar_values(baseline, frame)
    } else {
      expect_live_nested_values(baseline, frame)
    }
    for (empty_sql in c(
      paste0("SELECT * FROM (", sql, ") q WHERE FALSE"),
      paste0(sql, " LIMIT 0"),
      paste0("WITH invented AS (", sql, ") SELECT * FROM invented OFFSET 1")
    )) {
      empty <- lake_query(empty_sql, as = "arrow", con = con)
      testthat::expect_equal(empty$num_rows, 0, tolerance = 0)
      testthat::expect_true(baseline$schema$Equals(
        empty$schema,
        check_metadata = TRUE
      ))
      testthat::expect_equal(
        DBI::dbGetQuery(con, empty_sql),
        frame[0, ],
        ignore_attr = "tzone"
      )
    }
  }
})
