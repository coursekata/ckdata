test_that("saved results reuse fresh files and refresh on request", {
  local_saved_policy()
  fixture <- local_saved_tbl(data.frame(x = 1:3))
  q <- fixture$query
  dir <- withr::local_tempdir()

  first <- collect_saved(q, dir = dir)
  expect_identical(first$x, 1:3)
  expect_length(list.files(dir, pattern = "\\.parquet$"), 1L)

  # Change the underlying data; the same query should still return the cached rows.
  DBI::dbExecute(fixture$source, "INSERT INTO t (x) VALUES (4)")
  cached <- collect_saved(q, dir = dir)
  expect_identical(cached, first)
  expect_length(fixture$calls$queries, 1L)

  # refresh = TRUE re-runs and sees the new row.
  refreshed <- collect_saved(q, refresh = TRUE, dir = dir)
  expect_identical(refreshed$x, 1:4)
  expect_length(fixture$calls$queries, 2L)
})

test_that("saved results and snapshots publish in directories containing spaces", {
  local_saved_policy()
  fixture <- local_saved_tbl(data.frame(x = 1L))
  dir <- fs::path(withr::local_tempdir(), "saved results", "nested directory")

  saved <- collect_saved(fixture$query, dir = dir)
  snapshot <- collect_saved(fixture$query, snapshot = "baseline", dir = dir)
  DBI::dbExecute(fixture$source, "INSERT INTO t VALUES (2)")
  refreshed <- collect_saved(fixture$query, refresh = TRUE, dir = dir)

  expect_identical(saved$x, 1L)
  expect_identical(snapshot, saved)
  expect_identical(refreshed$x, 1:2)
  expect_identical(
    collect_saved(fixture$query, snapshot = "baseline", dir = dir),
    snapshot
  )
  expect_length(fixture$calls$queries, 3L)
  expect_length(fs::dir_ls(dir, all = TRUE), 2L)
})

test_that("cached and fresh frames retain the current query's grouping", {
  local_saved_policy()
  fixture <- local_saved_tbl(data.frame(team = c("a", "a", "b"), x = 1:3))
  query <- fixture$query
  dir <- withr::local_tempdir()
  plain <- collect_saved(query, dir = dir)
  grouped <- dplyr::group_by(query, team)
  expected <- dplyr::tbl(fixture$source, "t") |>
    dplyr::group_by(team) |>
    dplyr::collect()

  # Grouping alone does not change SQL, so this first call reuses the plain cache.
  for (refresh in c(FALSE, TRUE)) {
    actual <- collect_saved(grouped, refresh = refresh, dir = dir)
    expect_identical(actual, expected)
    expect_equal(
      dplyr::summarise(actual, n = dplyr::n()),
      dplyr::summarise(expected, n = dplyr::n())
    )
  }
  expect_identical(collect_saved(query, dir = dir), plain)
})

test_that("fresh saved results and snapshots survive reconnecting to the same source", {
  local_saved_policy()
  fixture <- local_native_connection()
  query <- dplyr::tbl(fixture$con, "types") |> dplyr::select(id, big)
  directory <- withr::local_tempdir()
  saved <- collect_saved(query, dir = directory)
  snapshot <- collect_saved(query, snapshot = "baseline", dir = directory)
  expect_length(fixture$calls$queries, 2L)
  DBI::dbDisconnect(fixture$con)

  con <- DBI::dbConnect(
    ckdata(),
    workgroup = "synthetic",
    schema = "fixture",
    role_arn = NULL
  )
  withr::defer(DBI::dbDisconnect(con))
  rebuilt <- dplyr::tbl(con, "types") |> dplyr::select(id, big)
  testthat::local_mocked_bindings(
    native_query = function(...) {
      rlang::abort("a reusable saved result must not submit a query")
    },
    .package = "ckdata"
  )
  expect_identical(collect_saved(rebuilt, dir = directory), saved)
  expect_identical(
    collect_saved(rebuilt, snapshot = "baseline", dir = directory),
    snapshot
  )
  expect_length(list.files(directory, pattern = "[.]parquet$"), 2L)
})

test_that("a failed publication returns fresh data and preserves the previous file", {
  local_saved_policy()
  fixture <- local_saved_tbl(data.frame(x = 1:3))
  query <- fixture$query
  dir <- withr::local_tempdir()
  previous <- collect_saved(query, dir = dir)
  DBI::dbExecute(fixture$source, "INSERT INTO t (x) VALUES (4)")
  testthat::local_mocked_bindings(
    file_move = function(path, new_path) {
      expect_true(file.exists(new_path))
      expect_identical(
        tibble::as_tibble(arrow::read_parquet(new_path)),
        previous
      )
      rlang::abort("synthetic cache publication failure")
    },
    .package = "fs"
  )
  expect_warning(
    fresh <- collect_saved(query, refresh = TRUE, dir = dir),
    class = "ckdata_save_failed"
  )
  expect_identical(fresh$x, 1:4)
  expect_identical(collect_saved(query, dir = dir), previous)
  expect_length(list.files(dir, pattern = "[.]parquet$"), 1L)
})

test_that("a cache hit is silent unless ckdata.verbose is TRUE", {
  local_saved_policy()
  withr::local_options(ckdata.verbose = FALSE)
  q <- local_saved_tbl(data.frame(x = 1:3))$query
  dir <- withr::local_tempdir()
  collect_saved(q, dir = dir)

  expect_no_message(collect_saved(q, dir = dir))

  withr::local_options(ckdata.verbose = TRUE)
  expect_message(collect_saved(q, dir = dir), "reading saved result")
})

test_that("a cache built while exploring does not hand flagged columns to publish mode", {
  local_saved_policy()
  fixture <- local_saved_tbl(
    data.frame(class_id = 1:2, is_test = FALSE),
    "int_classes"
  )
  q <- fixture$query
  dir <- withr::local_tempdir()
  collect_saved(q, dir = dir)

  withr::local_options(
    ckdata.mode = "publish",
    ckdata.allow = list(int_classes = "class_id")
  )
  expect_warning(
    out <- collect_saved(q, dir = dir),
    class = "ckdata_flagged_columns"
  )
  expect_named(out, "class_id")
  expect_identical(out$class_id, 1:2)
  expect_match(
    tail(fixture$calls$queries, 1),
    'SELECT "class_id"',
    fixed = TRUE
  )
})

test_that("collection checks column policy once on both hits and misses", {
  local_saved_policy()
  fixture <- local_saved_tbl(
    data.frame(class_id = 1:2, is_test = FALSE),
    "int_classes"
  )
  q <- fixture$query
  dir <- withr::local_tempdir()
  withr::local_options(ckdata.mode = "audit")

  expect_length(
    testthat::capture_warnings(first <- collect_saved(q, dir = dir)),
    1L
  )
  DBI::dbExecute(fixture$source, "INSERT INTO int_classes VALUES (3, 0)")
  expect_length(
    testthat::capture_warnings(second <- collect_saved(q, dir = dir)),
    1L
  )

  # Audit mode leaves the query as it is, so the second call is served from the first call's cache.
  expect_equal(second, first)
  expect_length(fixture$calls$queries, 1L)
})

test_that("independent generic DBI connections never reuse each other's rows", {
  local_saved_policy()
  first <- local_memory_tbl(data.frame(x = 1L))
  second <- local_memory_tbl(data.frame(x = 2L))
  dir <- file.path(withr::local_tempdir(), "cache")
  expect_error(collect_saved(first, dir = dir), "supply `source`")
  expect_identical(
    collect_saved(first, dir = dir, source = list(database = "invented-a"))$x,
    1L
  )
  expect_identical(
    collect_saved(second, dir = dir, source = list(database = "invented-b"))$x,
    2L
  )
  expect_identical(
    collect_saved(first, dir = dir, source = list(database = "invented-a"))$x,
    1L
  )
  expect_length(list.files(dir, pattern = "[.]parquet$"), 2L)
})

test_that("explicit generic source contexts allow stable reuse and isolate access", {
  local_saved_policy()
  query <- local_memory_tbl(data.frame(x = 1L))
  dir <- withr::local_tempdir()
  source <- list(database = "invented", principal = "reader-a")
  first <- collect_saved(query, dir = dir, source = source)
  DBI::dbExecute(dbplyr::remote_con(query), "INSERT INTO t VALUES (2)")
  withr::local_options(
    ckdata.mode = "publish",
    ckdata.allow = list(unrelated = "x")
  )
  expect_identical(collect_saved(query, dir = dir, source = source), first)
  expect_identical(
    collect_saved(
      query,
      dir = dir,
      source = list(database = "invented", principal = "reader-b")
    )$x,
    1:2
  )
})

test_that("generic DBI saves do not inherit lake policy from a matching table path", {
  fixture <- local_native_connection()
  local_lake_session(fixture$con)
  table <- DBI::Id(schema = "main", table = "int_example")
  registered <- dplyr::tbl(fixture$con, table, vars = c("id", "value"))
  register_lake_table(registered)
  source <- local_memory_con(data.frame(id = 9L, value = 10L), "int_example")
  query <- dplyr::tbl(source, dbplyr::sql('SELECT * FROM "main"."int_example"'))
  withr::local_options(
    ckdata.mode = "publish",
    ckdata.allow = list(int_example = "id")
  )
  expect_no_warning(
    frame <- collect_saved(
      query,
      dir = withr::local_tempdir(),
      source = list(database = "unrelated")
    )
  )
  expect_identical(frame, tibble::tibble(id = 9L, value = 10L))
})

test_that("snapshots freeze automatically or by label and default to the working directory", {
  fixture <- local_saved_tbl(data.frame(x = 1:3))
  q <- fixture$query
  dir <- withr::local_tempdir()
  withr::local_dir(dir)
  withr::local_options(ckdata.snapshot_dir = NULL)
  original <- collect_saved(q, snapshot = TRUE)
  baseline <- collect_saved(q, snapshot = "baseline")
  DBI::dbExecute(fixture$source, "INSERT INTO t VALUES (4)")
  expect_identical(collect_saved(q, snapshot = TRUE), original)
  expect_identical(collect_saved(q, snapshot = "baseline"), baseline)
  expect_length(fixture$calls$queries, 2L)
  expect_length(list.files(dir, pattern = "^snapshot-.*[.]parquet$"), 2L)
  expect_identical(collect_saved(q, snapshot = "next")$x, 1:4)
  expect_identical(
    collect_saved(q, snapshot = "baseline", dir = "elsewhere")$x,
    1:4
  )
})

test_that("concurrent snapshot misses retain and return the first publication", {
  fixture <- local_saved_tbl(data.frame(x = 1L))
  query <- fixture$query
  dir <- withr::local_tempdir()
  competing <- FALSE
  testthat::local_mocked_bindings(
    dbGetQueryArrow = function(conn, statement, ...) {
      if (!competing) {
        competing <<- TRUE
        expect_identical(
          collect_saved(query, snapshot = "baseline", dir = dir)$x,
          2L
        )
        value <- 1L
      } else {
        value <- 2L
      }
      nanoarrow::as_nanoarrow_array_stream(arrow::Table$create(x = value))
    },
    .package = "DBI"
  )
  expect_identical(collect_saved(query, snapshot = "baseline", dir = dir)$x, 2L)
  expect_identical(collect_saved(query, snapshot = "baseline", dir = dir)$x, 2L)
  expect_length(list.files(dir, all.files = TRUE, no.. = TRUE), 1L)
})

test_that("an unreadable concurrent snapshot is preserved and reported", {
  fixture <- local_saved_tbl(data.frame(x = 1L))
  dir <- withr::local_tempdir()
  link <- fs::link_create
  testthat::local_mocked_bindings(
    link_create = function(path, new_path, symbolic) {
      writeLines("broken competing snapshot", new_path)
      link(path, new_path, symbolic = symbolic)
    },
    .package = "fs"
  )
  expect_no_warning(expect_error(
    collect_saved(fixture$query, snapshot = "baseline", dir = dir),
    class = "ckdata_snapshot_unreadable"
  ))
  files <- list.files(dir, all.files = TRUE, no.. = TRUE, full.names = TRUE)
  expect_length(files, 1L)
  expect_identical(readLines(files), "broken competing snapshot")
})

test_that("snapshot and normal destinations remain separate and configurable", {
  fixture <- local_saved_tbl(data.frame(x = 1L))
  dir <- withr::local_tempdir()
  withr::local_options(list(
    ckdata.saved_dir = file.path(dir, "normal"),
    ckdata.snapshot_dir = file.path(dir, "snapshots")
  ))
  collect_saved(fixture$query)
  collect_saved(fixture$query, snapshot = TRUE)
  collect_saved(
    fixture$query,
    snapshot = TRUE,
    dir = file.path(dir, "explicit")
  )
  expect_true(file.exists(list.files(
    file.path(dir, "normal"),
    full.names = TRUE
  )))
  expect_true(file.exists(list.files(
    file.path(dir, "snapshots"),
    full.names = TRUE
  )))
  expect_true(file.exists(list.files(
    file.path(dir, "explicit"),
    full.names = TRUE
  )))
  withr::local_options(ckdata.saved_dir = NULL)
  expect_identical(
    fs::path_norm(fs::path_dir(saved_path(NULL, FALSE, NULL, "key"))),
    fs::path_norm(fs::path(tempdir(), "ckdata", "saved"))
  )
})

test_that("unconfigured freshness fetches anew even in a persistent directory", {
  withr::local_options(ckdata.freshness = NULL)
  withr::local_envvar(c(CKDATA_FRESHNESS_AT = NA, CKDATA_FRESHNESS_TZ = NA))
  fixture <- local_saved_tbl(data.frame(x = 1L))
  dir <- withr::local_tempdir()
  collect_saved(fixture$query, dir = dir)
  DBI::dbExecute(fixture$source, "INSERT INTO t VALUES (2)")
  expect_identical(collect_saved(fixture$query, dir = dir)$x, 1:2)
  expect_length(fixture$calls$queries, 2L)
  expect_length(list.files(dir), 1L)
})

test_that("failed queries return stale rows with a warning or propagate without saved rows", {
  local_saved_policy()
  fixture <- local_saved_tbl(data.frame(x = 1:2))
  dir <- withr::local_tempdir()
  previous <- collect_saved(fixture$query, dir = dir)
  DBI::dbExecute(fixture$source, "DROP TABLE t")
  expect_warning(
    stale <- collect_saved(fixture$query, refresh = TRUE, dir = dir),
    "stale saved data.*seconds old",
    class = "ckdata_saved_stale"
  )
  expect_identical(stale, previous)
  expect_error(
    collect_saved(fixture$query, dir = withr::local_tempdir()),
    "no such table"
  )
})

test_that("a failed initial save still returns the fresh rows", {
  fixture <- local_saved_tbl(data.frame(x = 1L))
  directory <- withr::local_tempdir()
  path <- file.path(directory, "not-a-directory")
  writeLines("invented", path)
  expect_warning(
    frame <- collect_saved(fixture$query, snapshot = TRUE, dir = path),
    "not saved",
    class = "ckdata_save_failed"
  )
  expect_identical(frame$x, 1L)
  expect_identical(readLines(path), "invented")
})

test_that("unreadable files warn and refetch rather than returning broken data", {
  local_saved_policy()
  fixture <- local_saved_tbl(data.frame(x = 1L))
  dir <- withr::local_tempdir()
  first <- collect_saved(fixture$query, dir = dir)
  path <- list.files(dir, full.names = TRUE)
  writeLines("invented invalid parquet", path)
  expect_warning(
    second <- collect_saved(fixture$query, dir = dir),
    class = "ckdata_saved_unreadable"
  )
  expect_identical(second, first)
  expect_length(fixture$calls$queries, 2L)
})

test_that("an unreadable existing snapshot is preserved without another query", {
  fixture <- local_saved_tbl(data.frame(x = 1L))
  dir <- withr::local_tempdir()
  collect_saved(fixture$query, snapshot = "baseline", dir = dir)
  path <- list.files(dir, full.names = TRUE)
  contents <- charToRaw("invented unreadable snapshot\n")
  writeBin(contents, path)
  expect_error(
    collect_saved(fixture$query, snapshot = "baseline", dir = dir),
    class = "ckdata_snapshot_unreadable"
  )
  expect_identical(readBin(path, "raw", n = length(contents) + 1L), contents)
  expect_length(fixture$calls$queries, 1L)
})

test_that("snapshot names and contradictory policies fail before execution", {
  fixture <- local_saved_tbl(data.frame(x = 1L))
  for (snapshot in list(NA, 1, "", "../baseline", "folder/file")) {
    expect_error(collect_saved(fixture$query, snapshot = snapshot))
  }
  expect_error(
    collect_saved(fixture$query, snapshot = TRUE, refresh = TRUE),
    "snapshot cannot"
  )
  expect_error(
    collect_saved(fixture$query, snapshot = "baseline", freshness = "always"),
    "snapshot cannot"
  )
  expect_length(fixture$calls$queries, 0L)
})

test_that("Parquet saves retain original Arrow precision and typed empty schemas", {
  fixture <- local_native_connection()
  table <- arrow::Table$create(
    big = arrow::Array$create(c(
      "-9223372036854775808",
      "9007199254740993",
      NA
    ))$cast(arrow::int64()),
    decimal = arrow::Array$create(c(
      "12345678901234567890.1234",
      "0.0001",
      NA
    ))$cast(arrow::decimal128(38, 4)),
    nested = arrow::Array$create(
      list(c("1.10", NA), character(), NULL),
      type = arrow::list_of(arrow::utf8())
    )$cast(arrow::list_of(arrow::decimal128(20, 2)))
  )
  testthat::local_mocked_bindings(
    dbGetQueryArrow = function(...) nanoarrow::as_nanoarrow_array_stream(table),
    .package = "DBI"
  )
  query <- dplyr::tbl(fixture$con, "invented", vars = names(table))
  for (empty in c(FALSE, TRUE)) {
    if (empty) {
      table <- table$Slice(0, 0)
    }
    dir <- withr::local_tempdir()
    first <- collect_saved(query, snapshot = TRUE, dir = dir)
    saved <- arrow::read_parquet(
      list.files(dir, full.names = TRUE),
      as_data_frame = FALSE
    )
    expect_true(table$schema$Equals(saved$schema, check_metadata = FALSE))
    for (name in names(table)) {
      expect_true(table[[name]]$Equals(saved[[name]]))
    }
    expect_identical(collect_saved(query, snapshot = TRUE, dir = dir), first)
  }
})

test_that("snapshot labels never mix distinct SQL or source contexts", {
  fixture <- local_saved_tbl(data.frame(x = 1:2))
  dir <- withr::local_tempdir()
  query <- fixture$query
  first <- collect_saved(query, snapshot = "baseline", dir = dir)
  selected <- dplyr::filter(query, x == 2L)
  expect_identical(
    collect_saved(selected, snapshot = "baseline", dir = dir)$x,
    2L
  )
  expect_identical(
    collect_saved(
      query,
      snapshot = "baseline",
      dir = dir,
      source = list(tenant = "another")
    ),
    first
  )
  expect_length(fixture$calls$queries, 3L)
  expect_length(
    list.files(dir, pattern = "^snapshot-baseline-.*[.]parquet$"),
    3L
  )
})
