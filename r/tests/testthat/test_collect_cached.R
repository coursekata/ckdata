test_that("collect_cached writes a cache, then serves it without re-querying", {
  fixture <- local_cached_tbl(data.frame(x = 1:3))
  q <- fixture$query
  dir <- withr::local_tempdir()

  first <- collect_cached(q, dir = dir)
  expect_identical(first$x, 1:3)
  expect_length(list.files(dir, pattern = "\\.parquet$"), 1L)

  # Change the underlying data; the same query should still return the cached rows.
  DBI::dbExecute(fixture$source, "INSERT INTO t (x) VALUES (4)")
  cached <- collect_cached(q, dir = dir)
  expect_identical(cached, first)
  expect_length(fixture$calls$queries, 1L)

  # refresh = TRUE re-runs and sees the new row.
  refreshed <- collect_cached(q, refresh = TRUE, dir = dir)
  expect_identical(refreshed$x, 1:4)
  expect_length(fixture$calls$queries, 2L)
})

test_that("cached and fresh frames retain the current query's grouping", {
  fixture <- local_cached_tbl(data.frame(team = c("a", "a", "b"), x = 1:3))
  query <- fixture$query
  dir <- withr::local_tempdir()
  plain <- collect_cached(query, dir = dir)
  grouped <- dplyr::group_by(query, team)
  expected <- dplyr::tbl(fixture$source, "t") |>
    dplyr::group_by(team) |>
    dplyr::collect()

  # Grouping alone does not change SQL, so this first call reuses the plain cache.
  for (refresh in c(FALSE, TRUE)) {
    actual <- collect_cached(grouped, refresh = refresh, dir = dir)
    expect_identical(actual, expected)
    expect_equal(dplyr::summarise(actual, n = dplyr::n()), dplyr::summarise(expected, n = dplyr::n()))
  }
  expect_identical(collect_cached(query, dir = dir), plain)
})

test_that("a failed refresh leaves the previous cache readable", {
  fixture <- local_cached_tbl(data.frame(x = 1:3))
  query <- fixture$query
  dir <- withr::local_tempdir()
  previous <- collect_cached(query, dir = dir)
  DBI::dbExecute(fixture$source, "INSERT INTO t (x) VALUES (4)")
  testthat::local_mocked_bindings(
    file_move = function(path, new_path) {
      expect_true(file.exists(new_path))
      expect_identical(tibble::as_tibble(arrow::read_parquet(new_path)), previous)
      rlang::abort("synthetic cache publication failure")
    },
    .package = "fs"
  )
  expect_error(collect_cached(query, refresh = TRUE, dir = dir), "synthetic cache publication failure")
  expect_identical(collect_cached(query, dir = dir), previous)
  expect_length(list.files(dir, pattern = "[.]parquet$"), 1L)
})

test_that("a cache hit is silent unless ckdata.verbose is TRUE", {
  q <- local_cached_tbl(data.frame(x = 1:3))$query
  dir <- withr::local_tempdir()
  collect_cached(q, dir = dir)

  expect_no_message(collect_cached(q, dir = dir))

  withr::local_options(ckdata.verbose = TRUE)
  expect_message(collect_cached(q, dir = dir), "reading cached result")
})

test_that("a cache built while exploring does not hand flagged columns to publish mode", {
  fixture <- local_cached_tbl(data.frame(class_id = 1:2, is_test = FALSE), "int_classes")
  q <- fixture$query
  dir <- withr::local_tempdir()
  collect_cached(q, dir = dir)

  withr::local_options(ckdata.mode = "publish", ckdata.allow = list(int_classes = "class_id"))
  expect_warning(out <- collect_cached(q, dir = dir), class = "ckdata_flagged_columns")
  expect_named(out, "class_id")
  expect_identical(out$class_id, 1:2)
  expect_match(tail(fixture$calls$queries, 1), 'SELECT "class_id"', fixed = TRUE)
})

test_that("collect_cached checks a query once, whether or not the cache has it", {
  fixture <- local_cached_tbl(data.frame(class_id = 1:2, is_test = FALSE), "int_classes")
  q <- fixture$query
  dir <- withr::local_tempdir()
  withr::local_options(ckdata.mode = "audit")

  expect_length(testthat::capture_warnings(first <- collect_cached(q, dir = dir)), 1L)
  DBI::dbExecute(fixture$source, "INSERT INTO int_classes VALUES (3, 0)")
  expect_length(testthat::capture_warnings(second <- collect_cached(q, dir = dir)), 1L)

  # Audit mode leaves the query as it is, so the second call is served from the first call's cache.
  expect_equal(second, first)
  expect_length(fixture$calls$queries, 1L)
})

test_that("collect_cached rejects independent SQLite databases before policy or execution", {
  first <- local_memory_tbl(data.frame(x = 1L))
  second <- local_memory_tbl(data.frame(x = 2L))
  expect_identical(dplyr::collect(first)$x, 1L)
  expect_identical(dplyr::collect(second)$x, 2L)
  dir <- file.path(withr::local_tempdir(), "cache")
  testthat::local_mocked_bindings(
    apply_column_policy = function(...) rlang::abort("column policy must not run"),
    .package = "ckdata"
  )
  testthat::local_mocked_bindings(
    dbGetQueryArrow = function(...) rlang::abort("query must not run"),
    .package = "DBI"
  )

  for (query in list(first, second)) {
    expect_error(collect_cached(query, dir = dir), "`query` must use a ckdata connection")
  }
  expect_false(dir.exists(dir))
})
