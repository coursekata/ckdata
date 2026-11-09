dialect_connection <- function() {
  methods::new("CkConnection", state = rlang::new_environment())
}

expect_projected_sql <- function(sql, expressions) {
  sql <- stringr::str_squish(as.character(sql))
  present <- purrr::imap_lgl(expressions, function(expression, name) {
    stringr::str_detect(
      sql,
      stringr::fixed(paste0(expression, ' AS "', name, '"'))
    )
  })
  expect_identical(
    present,
    rlang::set_names(rep(TRUE, length(expressions)), names(expressions))
  )
}

test_that("grepl translates regex, case-insensitive and literal matching", {
  con <- dialect_connection()
  expect_identical(
    as.character(dbplyr::translate_sql(grepl("^a", text), con = con)),
    'CASE WHEN \'^a\' IS NULL THEN CAST(NULL AS BOOLEAN) ELSE COALESCE(REGEXP_LIKE("text", \'^a\'), FALSE) END'
  )
  expect_match(
    as.character(dbplyr::translate_sql(
      grepl("^a", text, ignore.case = TRUE),
      con = con
    )),
    "CONCAT('(?i)', '^a')",
    fixed = TRUE
  )
  expect_match(
    as.character(dbplyr::translate_sql(
      grepl("a.b", text, fixed = TRUE),
      con = con
    )),
    'STRPOS("text", \'a.b\') > 0',
    fixed = TRUE
  )
  expect_match(
    as.character(dbplyr::translate_sql(
      grepl("A.B", text, fixed = TRUE, ignore.case = TRUE),
      con = con
    )),
    'STRPOS(LOWER("text"), LOWER(\'A.B\')) > 0',
    fixed = TRUE
  )
  expect_error(
    dbplyr::translate_sql(grepl("x", text, perl = TRUE), con = con),
    "Unicode Java"
  )
  expect_error(
    dbplyr::translate_sql(grepl("x", text, useBytes = TRUE), con = con),
    "Unicode Java"
  )
  expect_error(
    dbplyr::translate_sql(grepl("x", text, ignore.case = NA), con = con),
    "`ignore.case` must be"
  )
})

test_that("floor_date uses native units and honors the R week start", {
  con <- dialect_connection()
  expect_identical(
    as.character(dbplyr::translate_sql(floor_date(stamp), con = con)),
    'DATE_TRUNC(\'second\', "stamp")'
  )
  expect_identical(
    as.character(dbplyr::translate_sql(floor_date(stamp, "months"), con = con)),
    'DATE_TRUNC(\'month\', "stamp")'
  )
  expect_identical(
    as.character(dbplyr::translate_sql(
      floor_date(stamp, "week", week_start = 1),
      con = con
    )),
    'DATE_TRUNC(\'week\', "stamp")'
  )
  expect_identical(
    as.character(dbplyr::translate_sql(
      floor_date(stamp, "week", week_start = 7),
      con = con
    )),
    'DATE_ADD(\'day\', -1, DATE_TRUNC(\'week\', DATE_ADD(\'day\', 1, "stamp")))'
  )
  withr::local_options(lubridate.week.start = 2)
  expect_match(
    as.character(dbplyr::translate_sql(floor_date(stamp, "week"), con = con)),
    "DATE_ADD('day', 6,",
    fixed = TRUE
  )
  expect_error(
    dbplyr::translate_sql(floor_date(stamp, "week", week_start = 0), con = con),
    "week_start"
  )
  expect_error(
    dbplyr::translate_sql(floor_date(stamp, "2 months"), con = con),
    "must be one of"
  )
})

test_that("Athena scalar translations handle NULL and signed infinity", {
  con <- dialect_connection()
  expect_identical(
    as.character(dbplyr::translate_sql(is.infinite(x), con = con)),
    'COALESCE(IS_INFINITE("x"), FALSE)'
  )
  expect_identical(
    as.character(dbplyr::translate_sql(is.finite(x), con = con)),
    'COALESCE(IS_FINITE("x"), FALSE)'
  )
  expect_identical(
    as.character(dbplyr::translate_sql(is.nan(x), con = con)),
    'COALESCE(IS_NAN("x"), FALSE)'
  )
  expect_identical(
    as.character(dbplyr::translate_sql(month(ts), con = con)),
    'MONTH("ts")'
  )
  expect_identical(
    as.character(dbplyr::translate_sql(as.character(x), con = con)),
    'CAST("x" AS VARCHAR)'
  )
  expect_error(
    dbplyr::translate_sql(month(ts, label = TRUE), con = con),
    "label = TRUE"
  )
})

test_that("typed date/time literals are shared by DBI and dbplyr", {
  con <- dialect_connection()
  date <- as.Date(c("2026-11-03", NA))
  stamp <- as.POSIXct(c("2026-11-03 12:34:56.123", NA), tz = "UTC")
  expect_identical(
    as.character(DBI::dbQuoteLiteral(con, date)),
    c("DATE '2026-11-03'", "CAST(NULL AS DATE)")
  )
  expect_identical(
    as.character(DBI::dbQuoteLiteral(con, stamp)),
    c("TIMESTAMP '2026-11-03 12:34:56.123'", "CAST(NULL AS TIMESTAMP)")
  )
  expect_identical(
    as.character(dbplyr::translate_sql(x > !!date[1], con = con)),
    '"x" > DATE \'2026-11-03\''
  )
  expect_identical(
    as.character(dbplyr::translate_sql(x > !!stamp[1], con = con)),
    '"x" > TIMESTAMP \'2026-11-03 12:34:56.123\''
  )
  withr::local_timezone("America/Los_Angeles")
  expect_identical(
    as.character(DBI::dbQuoteLiteral(con, stamp)),
    c("TIMESTAMP '2026-11-03 12:34:56.123'", "CAST(NULL AS TIMESTAMP)")
  )
})

test_that("approximation is explicit for grouped and window summaries", {
  con <- dialect_connection()
  for (window in c(FALSE, TRUE)) {
    expect_error(
      dbplyr::translate_sql(median(x), con = con, window = window),
      "approx_median"
    )
    expect_error(
      dbplyr::translate_sql(quantile(x), con = con, window = window),
      "approx_quantile"
    )
    sql <- dbplyr::translate_sql(
      approx_quantile(x, 0.95, na.rm = TRUE),
      con = con,
      window = window
    )
    expect_match(
      as.character(sql),
      'APPROX_PERCENTILE("x", 0.95)',
      fixed = TRUE
    )
    expect_identical(grepl("OVER", sql), window)
    median <- dbplyr::translate_sql(
      approx_median(x, na.rm = TRUE),
      con = con,
      window = window
    )
    expect_match(
      as.character(median),
      'APPROX_PERCENTILE("x", 0.5)',
      fixed = TRUE
    )
  }
  expect_error(
    dbplyr::translate_sql(approx_quantile(x, 2), con = con, window = FALSE),
    "`probs` must be"
  )
})

test_that("dbplyr renders composed joins, summaries and windows with the public dialect", {
  con <- dialect_connection()
  left <- dbplyr::lazy_frame(id = 1L, value = 1, grp = "a", con = con)
  right <- dbplyr::lazy_frame(id = 1L, label = "b", con = con)
  joined <- dplyr::left_join(left, right, by = "id")
  query <- joined |>
    dplyr::filter(value > 0) |>
    dplyr::group_by(grp) |>
    dplyr::summarise(
      total = sum(value, na.rm = TRUE),
      count = dplyr::n(),
      .groups = "drop"
    )
  sql <- as.character(dbplyr::sql_render(query))
  for (fragment in c("LEFT JOIN", "WHERE", "GROUP BY", "SUM(", "COUNT(*)")) {
    expect_match(sql, fragment, fixed = TRUE)
  }
  window <- left |>
    dplyr::group_by(grp) |>
    dplyr::mutate(r = dplyr::row_number())
  expect_match(
    as.character(dbplyr::sql_render(window)),
    "PARTITION BY",
    fixed = TRUE
  )
})

test_that("inline lookup values use Athena casts without submitting a query", {
  fixture <- local_native_connection()
  values <- data.frame(
    id = c(1L, 2L),
    label = c("O'Brien", NA_character_),
    flag = c(TRUE, NA),
    big = bit64::as.integer64(c("9007199254740993", NA))
  )
  query <- dbplyr::copy_inline(fixture$con, values)
  sql <- as.character(dbplyr::sql_render(query))
  expect_match(sql, 'CAST("label" AS VARCHAR)', fixed = TRUE)
  expect_match(sql, 'CAST("flag" AS BOOLEAN)', fixed = TRUE)
  expect_match(sql, 'CAST("big" AS BIGINT)', fixed = TRUE)
  expect_match(sql, "'O''Brien'", fixed = TRUE)
  expect_match(sql, "9007199254740993", fixed = TRUE)
  expect_length(fixture$calls$queries, 0L)
  expect_equal(fixture$calls$metadata, 0L)
})

test_that("inline mixed doubles are typed before VALUES inference and map NaN to NULL", {
  fixture <- local_native_connection()
  values <- data.frame(
    id = 1:6,
    value = c(1.25, Inf, -Inf, NA_real_, NaN, -4.5)
  )
  query <- dbplyr::copy_inline(fixture$con, values)
  sql <- as.character(dbplyr::remote_query(query))
  literals <- c("1.25", "'Infinity'", "'-Infinity'", "NULL", "NULL", "-4.5")
  for (i in seq_along(literals)) {
    expect_match(
      sql,
      paste0("(", i, ", CAST(", literals[[i]], " AS DOUBLE))"),
      fixed = TRUE
    )
  }
  expect_match(sql, 'CAST("value" AS DOUBLE)', fixed = TRUE)
  overridden <- dbplyr::copy_inline(
    fixture$con,
    values,
    types = c(id = "BIGINT", value = "REAL")
  )
  expect_match(
    as.character(dbplyr::remote_query(overridden)),
    'CAST("value" AS REAL)',
    fixed = TRUE
  )
  expect_identical(query$lazy_query$x, values)
  expect_null(query$lazy_query$col_types)
  expect_identical(
    as.character(dbplyr::translate_sql(Inf, con = fixture$con)),
    "'Infinity'"
  )
  expect_identical(
    as.character(dbplyr::translate_sql(NaN, con = fixture$con)),
    "NULL"
  )
  expect_length(fixture$calls$queries, 0L)
  expect_equal(fixture$calls$metadata, 0L)
})

test_that("joining local lookup data inline does not need a remote table", {
  fixture <- local_native_connection()
  remote <- dplyr::tbl(fixture$con, "types")
  values <- data.frame(id = 1L, label = "invented")
  query <- dplyr::left_join(remote, values, by = "id", copy = "inline")
  sql <- as.character(dbplyr::sql_render(query))
  expect_match(sql, "LEFT JOIN", fixed = TRUE)
  expect_match(sql, "VALUES", fixed = TRUE)
  expect_match(sql, 'CAST("label" AS VARCHAR)', fixed = TRUE)
  expect_length(fixture$calls$queries, 0L)
  expect_equal(fixture$calls$metadata, 1L)
})

test_that("projected missing literals retain their R types before SQL rendering", {
  fixture <- local_native_connection()
  tbl <- dplyr::tbl(fixture$con, "types")
  query <- tbl |>
    dplyr::mutate(
      text = NA_character_,
      integer = NA_integer_,
      number = NA_real_,
      flag = NA,
      day = !!as.Date(NA),
      stamp = !!as.POSIXct(NA),
      big_missing = !!bit64::as.integer64(NA),
      inferred = id + NA,
      conditional = dplyr::if_else(id == 1, NA_integer_, 3L)
    ) |>
    dplyr::filter(id > 0)
  sql <- as.character(dbplyr::remote_query(query))
  types <- c(
    text = "VARCHAR",
    integer = "INTEGER",
    number = "DOUBLE",
    flag = "BOOLEAN",
    day = "DATE",
    stamp = "TIMESTAMP",
    big_missing = "BIGINT"
  )
  expect_projected_sql(
    sql,
    purrr::map_chr(types, function(type) paste0("CAST(NULL AS ", type, ")"))
  )
  expect_match(sql, '"id" + NULL', fixed = TRUE)
  expect_output(dplyr::show_query(query), "CAST\\(NULL AS VARCHAR\\)")
  expect_length(fixture$calls$queries, 0L)
  expect_match(
    as.character(dbplyr::remote_query(
      tbl |> dplyr::mutate(raw = dbplyr::sql("NULL"))
    )),
    "NULL AS",
    fixed = TRUE
  )
})

test_that("typed missing projections survive joins, unions and nested expressions", {
  fixture <- local_native_connection()
  query <- dplyr::tbl(fixture$con, "types") |>
    dplyr::select(id) |>
    dplyr::mutate(a = NA_real_)
  joined <- dplyr::left_join(query, query, by = "id")
  sql <- as.character(dbplyr::remote_query(joined))
  expect_length(
    regmatches(sql, gregexpr("CAST(NULL AS DOUBLE)", sql, fixed = TRUE))[[1]],
    2L
  )
  union <- dplyr::union_all(query, query)
  sql <- as.character(dbplyr::remote_query(union))
  expect_length(
    regmatches(sql, gregexpr("CAST(NULL AS DOUBLE)", sql, fixed = TRUE))[[1]],
    2L
  )
  query <- query |>
    dplyr::mutate(a = dplyr::if_else(id %in% c(1L, NA_integer_), NA_real_, 2.5))
  expect_match(
    as.character(dbplyr::remote_query(query)),
    "CAST(NULL AS INTEGER)",
    fixed = TRUE
  )
  expect_match(
    as.character(dbplyr::remote_query(query)),
    "CAST(NULL AS DOUBLE)",
    fixed = TRUE
  )
})

test_that("logical missing output contexts are typed without changing numeric inference", {
  fixture <- local_native_connection()
  query <- dplyr::tbl(fixture$con, "types") |>
    dplyr::mutate(
      parenthesized = (NA),
      coalesced = coalesce(NA, NA),
      conditional = dplyr::if_else(id > 0, NA, NA),
      reordered = ifelse(no = NA, test = id > 0, yes = NA),
      cases = dplyr::case_when(id > 0 ~ NA, .default = NA),
      cases_implicit = dplyr::case_when(id > 0 ~ (NA)),
      nested_coalesce = coalesce(dplyr::if_else(id > 0, NA, NA), NA),
      nested_conditional = dplyr::if_else(id > 0, coalesce(NA, NA), NA),
      nested_inferred = dplyr::if_else(id > 0, coalesce(NA, NA), 1L),
      inferred = dplyr::if_else(id > 0, NA, 1L),
      arithmetic = id + (NA)
    )
  sql <- as.character(dbplyr::remote_query(query))
  boolean <- "CAST(NULL AS BOOLEAN)"
  coalesced <- paste0("COALESCE(", boolean, ", ", boolean, ")")
  conditional <- function(yes, no) {
    paste0(
      'CASE WHEN ("id" > 0.0) THEN ',
      yes,
      ' WHEN NOT ("id" > 0.0) THEN ',
      no,
      " END"
    )
  }
  expect_projected_sql(
    sql,
    c(
      parenthesized = paste0("(", boolean, ")"),
      coalesced = coalesced,
      conditional = conditional(boolean, boolean),
      reordered = conditional(boolean, boolean),
      cases = paste0(
        'CASE WHEN ("id" > 0.0) THEN ',
        boolean,
        " ELSE ",
        boolean,
        " END"
      ),
      cases_implicit = paste0(
        'CASE WHEN ("id" > 0.0) THEN ((',
        boolean,
        ")) END"
      ),
      nested_coalesce = paste0(
        "COALESCE(",
        conditional(boolean, boolean),
        ", ",
        boolean,
        ")"
      ),
      nested_conditional = conditional(paste0("(", coalesced, ")"), boolean),
      nested_inferred = conditional("(COALESCE(NULL, NULL))", "1"),
      inferred = conditional("NULL", "1"),
      arithmetic = '"id" + (NULL)'
    )
  )
})

test_that("NaN retains dbplyr NULL semantics with a known double output type", {
  fixture <- local_native_connection()
  query <- dplyr::tbl(fixture$con, "types") |> dplyr::mutate(value = NaN)
  expect_match(
    as.character(dbplyr::remote_query(query)),
    'CAST(NULL AS DOUBLE) AS "value"',
    fixed = TRUE
  )
})

test_that("case_match retains missing output types and ignores its match input", {
  fixture <- local_native_connection()
  query <- dplyr::tbl(fixture$con, "types") |>
    dplyr::mutate(
      flag = dplyr::case_match(id, 1L ~ NA, .default = NA),
      named = dplyr::case_match(
        1L ~ (NA),
        .x = id,
        .default = coalesce(NA, NA)
      ),
      inferred = dplyr::case_match(id, 1L ~ NA, .default = 3L),
      text = dplyr::case_match(id, 1L ~ NA_character_, .default = "invented")
    )
  sql <- as.character(dbplyr::remote_query(query))
  expect_projected_sql(
    sql,
    c(
      flag = 'CASE WHEN ("id" IN (1)) THEN CAST(NULL AS BOOLEAN) ELSE CAST(NULL AS BOOLEAN) END',
      named = 'CASE WHEN ("id" IN (1)) THEN ((CAST(NULL AS BOOLEAN))) ELSE COALESCE(CAST(NULL AS BOOLEAN), CAST(NULL AS BOOLEAN)) END',
      inferred = 'CASE WHEN ("id" IN (1)) THEN NULL ELSE 3 END',
      text = 'CASE WHEN ("id" IN (1)) THEN CAST(NULL AS VARCHAR) ELSE \'invented\' END'
    )
  )
  expect_length(fixture$calls$queries, 0L)
})
