test_that("inline lookup values and joins execute without remote writes", {
  context <- local_live_context()
  con <- context$con
  values <- data.frame(
    id = c(1L, 2L, 99L, 100L, 101L, 102L),
    label = c("O'Brien", "", rep(NA_character_, 4)),
    flag = c(TRUE, FALSE, rep(NA, 4)),
    score = c(1.25, NA_real_, -4.5, Inf, -Inf, NaN),
    big = bit64::as.integer64(c(
      "9007199254740993",
      NA,
      "-4",
      rep(NA_character_, 3)
    )),
    day = as.Date(c("2026-11-03", NA, "1960-01-02", rep(NA_character_, 3))),
    stamp = as.POSIXct(
      c(
        "2026-11-03 12:34:56.123",
        NA,
        "1960-01-02 03:04:05.678",
        rep(NA_character_, 3)
      ),
      tz = "UTC"
    )
  )
  inline <- dbplyr::copy_inline(con, values)
  actual <- dplyr::collect(inline) |> dplyr::arrange(id)
  testthat::expect_identical(actual$id, values$id)
  testthat::expect_identical(actual$label, values$label)
  testthat::expect_identical(actual$flag, values$flag)
  testthat::expect_identical(
    actual$score,
    c(1.25, NA_real_, -4.5, Inf, -Inf, NA_real_)
  )
  testthat::expect_identical(as.character(actual$big), as.character(values$big))
  testthat::expect_identical(actual$day, values$day)
  expect_timestamp_equal(actual$stamp, values$stamp)
  empty <- dplyr::collect(dbplyr::copy_inline(con, values[0, ]))
  testthat::expect_identical(nrow(empty), 0L)
  testthat::expect_equal(empty, actual[0, ], ignore_attr = "tzone")
  nulls <- data.frame(
    text = NA_character_,
    number = NA_real_,
    integer = NA_integer_,
    flag = NA
  )
  actual <- dplyr::collect(dbplyr::copy_inline(con, nulls))
  testthat::expect_identical(nrow(actual), 1L)
  testthat::expect_type(actual$text, "character")
  testthat::expect_type(actual$number, "double")
  testthat::expect_type(actual$integer, "integer")
  testthat::expect_type(actual$flag, "logical")
  purrr::walk(actual, function(column) {
    testthat::expect_true(all(is.na(column)))
  })
  remote <- dplyr::tbl(con, "types") |> dplyr::select(id)
  joined <- dplyr::left_join(
    remote,
    dplyr::select(inline, id, label),
    by = "id"
  ) |>
    dplyr::collect() |>
    dplyr::arrange(id)
  direct <- dplyr::left_join(
    remote,
    values[c("id", "label")],
    by = "id",
    copy = "inline"
  ) |>
    dplyr::collect() |>
    dplyr::arrange(id)
  testthat::expect_identical(joined$id, c(1L, 2L))
  testthat::expect_identical(joined$label, c("O'Brien", ""))
  testthat::expect_identical(joined, direct)
  empty_join <- dplyr::inner_join(
    remote,
    dbplyr::copy_inline(con, values[0, c("id", "label")]),
    by = "id"
  ) |>
    dplyr::collect()
  testthat::expect_identical(nrow(empty_join), 0L)
  testthat::expect_equal(empty_join, joined[0, ])
})

test_that("dbplyr typed missing expressions retain populated and empty schemas", {
  context <- local_live_context()
  con <- context$con
  query <- dplyr::tbl(con, "types") |>
    dplyr::select(id) |>
    dplyr::mutate(
      text = NA_character_,
      integer = NA_integer_,
      number = NA_real_,
      flag = NA,
      day = !!as.Date(NA),
      stamp = !!as.POSIXct(NA),
      big = !!bit64::as.integer64(NA),
      inferred = id + NA,
      conditional = dplyr::if_else(id == 1, NA_integer_, 3L),
      parenthesized = (NA),
      coalesced = coalesce(NA, NA),
      logical_conditional = dplyr::if_else(id > 0, NA, NA),
      cases = dplyr::case_when(id > 0 ~ NA, .default = NA),
      nested_coalesce = coalesce(dplyr::if_else(id > 0, NA, NA), NA),
      nested_conditional = dplyr::if_else(id > 0, coalesce(NA, NA), NA),
      nan_missing = NaN,
      matched_flag = dplyr::case_match(id, 1L ~ NA, .default = NA),
      matched_nested = dplyr::case_match(
        1L ~ (NA),
        .x = id,
        .default = coalesce(NA, NA)
      ),
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
  purrr::walk(
    rows[c("text", "integer", "flag", "day", "stamp", "big")],
    function(column) {
      testthat::expect_true(all(is.na(column)))
    }
  )
  logical_columns <- c(
    "parenthesized",
    "coalesced",
    "logical_conditional",
    "cases",
    "nested_coalesce",
    "nested_conditional",
    "matched_flag",
    "matched_nested"
  )
  purrr::walk(rows[logical_columns], function(column) {
    testthat::expect_type(column, "logical")
    testthat::expect_true(all(is.na(column)))
  })
  testthat::expect_identical(rows$nan_missing, rep(NA_real_, 2L))
  testthat::expect_identical(rows$matched_number, c(NA_integer_, 3L))
  testthat::expect_identical(rows$inferred, rep(NA_integer_, 2L))
  testthat::expect_identical(rows$conditional, c(NA_integer_, 3L))
  full <- arrow::as_arrow_table(DBI::dbGetQueryArrow(
    con,
    as.character(dbplyr::remote_query(query))
  ))
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
})

test_that("managed column policy runs once and leaves direct DBI tables unchanged", {
  context <- local_live_context()
  con <- context$con
  deployment <- context$deployment
  changes <- tibble::tibble(
    table = "types",
    column = "s_null",
    new_table = "types",
    new_column = "s_empty",
    removed_on = "2099-01-01"
  )
  testthat::local_mocked_bindings(column_changes = changes, .package = "ckdata")
  validate_live_connection(lake_con(), context)
  managed <- lake_tbl("types")
  direct <- dplyr::tbl(
    con,
    DBI::Id(schema = deployment$database, table = "types")
  )
  withr::with_options(list(ckdata.mode = "audit"), {
    testthat::expect_length(
      testthat::capture_warnings(dplyr::collect(managed)),
      1L
    )
  })
  withr::with_options(list(ckdata.mode = "publish"), {
    testthat::expect_warning(
      published <- dplyr::collect(managed),
      class = "ckdata_flagged_columns"
    )
    testthat::expect_no_warning(explicit <- dplyr::collect(direct))
    testthat::expect_false(rlang::has_name(published, "s_null"))
    testthat::expect_true(rlang::has_name(explicit, "s_null"))
  })
})

test_that("managed Athena tables support discovery, filtering, joins and summaries", {
  context <- local_live_context()
  con <- context$con
  managed <- lake_con()
  validate_live_connection(managed, context)
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
})

test_that("Athena translations preserve finite, infinite, NaN and month behavior", {
  context <- local_live_context()
  con <- context$con
  literal <- dplyr::tbl(
    con,
    dbplyr::sql(
      "SELECT * FROM (VALUES (1, DOUBLE 'Infinity', DATE '2026-11-03'), (2, DOUBLE '-Infinity', DATE '2026-12-04'), (3, CAST(NULL AS DOUBLE), DATE '2026-01-01'), (4, DOUBLE 'NaN', DATE '2026-01-02'), (5, DOUBLE '1.25', DATE '2026-01-03')) t(id, value, stamp)"
    )
  )
  translated <- literal |>
    dplyr::mutate(
      infinite = is.infinite(value),
      finite = is.finite(value),
      nan = is.nan(value),
      month_value = month(stamp)
    ) |>
    dplyr::collect() |>
    dplyr::arrange(id)
  testthat::expect_identical(
    translated$infinite,
    c(TRUE, TRUE, FALSE, FALSE, FALSE)
  )
  testthat::expect_identical(
    translated$finite,
    c(FALSE, FALSE, FALSE, FALSE, TRUE)
  )
  testthat::expect_identical(
    translated$nan,
    c(FALSE, FALSE, FALSE, TRUE, FALSE)
  )
  testthat::expect_identical(
    as.integer(sort(translated$month_value)),
    c(1L, 1L, 1L, 11L, 12L)
  )
})

test_that("Athena approximate summaries execute and return values in the fixture range", {
  context <- local_live_context()
  con <- context$con
  approximate <- dplyr::tbl(con, "types") |>
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
})

test_that("Athena regex and date truncation translations preserve R semantics", {
  context <- local_live_context()
  con <- context$con
  functions <- dplyr::tbl(
    con,
    dbplyr::sql(
      "SELECT * FROM (VALUES (1, 'Alpha', TIMESTAMP '2026-11-01 12:34:56.789'), (2, 'a.b', TIMESTAMP '2026-11-02 00:00:00.001'), (3, CAST(NULL AS VARCHAR), CAST(NULL AS TIMESTAMP))) t(id, text_value, stamp)"
    )
  ) |>
    dplyr::mutate(
      regex = grepl("^a", text_value),
      insensitive = grepl("^a", text_value, ignore.case = TRUE),
      fixed = grepl("a.b", text_value, fixed = TRUE),
      missing_pattern = grepl(NA_character_, text_value),
      month_floor = floor_date(stamp, "month"),
      second_floor = floor_date(stamp),
      sunday_floor = floor_date(stamp, "week"),
      monday_floor = floor_date(stamp, "week", week_start = 1)
    ) |>
    dplyr::collect() |>
    dplyr::arrange(id)
  testthat::expect_identical(functions$regex, c(FALSE, TRUE, FALSE))
  testthat::expect_identical(functions$insensitive, c(TRUE, TRUE, FALSE))
  testthat::expect_identical(functions$fixed, c(FALSE, TRUE, FALSE))
  testthat::expect_type(functions$missing_pattern, "logical")
  testthat::expect_true(all(is.na(functions$missing_pattern)))
  expect_timestamp_equal(
    functions$month_floor,
    as.POSIXct(c("2026-11-01 00:00:00", "2026-11-01 00:00:00", NA), tz = "UTC"),
    tolerance = 0
  )
  expect_timestamp_equal(
    functions$second_floor,
    as.POSIXct(c("2026-11-01 12:34:56", "2026-11-02 00:00:00", NA), tz = "UTC"),
    tolerance = 0
  )
  expect_timestamp_equal(
    functions$sunday_floor,
    as.POSIXct(c("2026-11-01 00:00:00", "2026-11-01 00:00:00", NA), tz = "UTC"),
    tolerance = 0
  )
  expect_timestamp_equal(
    functions$monday_floor,
    as.POSIXct(c("2026-10-26 00:00:00", "2026-11-02 00:00:00", NA), tz = "UTC"),
    tolerance = 0
  )
})
