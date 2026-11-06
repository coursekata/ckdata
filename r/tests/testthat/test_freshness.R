test_that("daily boundaries use calendar days and refresh exactly at the scheduled time", {
  policy <- daily_at("07:00", "America/Los_Angeles")
  cases <- c(
    "2026-11-05 06:59:59" = "2026-11-04 07:00:00",
    "2026-11-05 07:00:00" = "2026-11-05 07:00:00",
    "2026-03-08 06:59:59" = "2026-03-07 07:00:00",
    "2026-03-08 07:00:00" = "2026-03-08 07:00:00",
    "2026-11-01 06:59:59" = "2026-10-31 07:00:00",
    "2026-11-01 07:00:00" = "2026-11-01 07:00:00"
  )
  for (i in seq_along(cases)) {
    now <- as.POSIXct(names(cases)[i], tz = policy$tz)
    expected <- as.POSIXct(cases[[i]], tz = policy$tz)
    expect_equal(daily_boundary(policy, now), expected)
    expect_true(saved_is_fresh(list(info = list(fetched_at = as.numeric(expected))), policy, NULL, now))
    expect_false(saved_is_fresh(list(info = list(fetched_at = as.numeric(expected) - 1)), policy, NULL, now))
  }
})

test_that("skipped and repeated schedule times use the documented DST occurrence", {
  skipped <- daily_at("02:30", "America/Los_Angeles")
  repeated <- daily_at("01:30", "America/Los_Angeles")
  expect_equal(daily_boundary(skipped, as.POSIXct("2026-03-08 11:00:00", tz = "UTC")), as.POSIXct("2026-03-08 10:00:00", tz = "UTC"), ignore_attr = TRUE)
  expect_equal(daily_boundary(repeated, as.POSIXct("2026-11-01 09:45:00", tz = "UTC")), as.POSIXct("2026-11-01 08:30:00", tz = "UTC"), ignore_attr = TRUE)
})

test_that("freshness follows collection, table, connection, option and environment precedence", {
  fixture <- local_native_connection()
  local_lake_session(fixture$con)
  environment <- daily_at("01:00", "UTC")
  application <- daily_at("02:00", "UTC")
  connection <- daily_at("03:00", "UTC")
  table <- daily_at("04:00", "UTC")
  collection <- daily_at("05:00", "UTC")
  withr::local_options(ckdata.freshness = NULL)
  withr::local_envvar(c(CKDATA_FRESHNESS_AT = "01:00", CKDATA_FRESHNESS_TZ = "UTC"))
  query <- lake_tbl("types")
  expect_identical(resolve_freshness(query, fixture$con, NULL), environment)
  withr::local_options(ckdata.freshness = application)
  expect_identical(resolve_freshness(query, fixture$con, NULL), application)
  fixture$con@state$freshness <- connection
  expect_identical(resolve_freshness(query, fixture$con, NULL), connection)
  explicit <- lake_tbl("types", freshness = table)
  expect_identical(resolve_freshness(explicit, fixture$con, NULL), table)
  expect_identical(resolve_freshness(explicit, fixture$con, collection), collection)
  expect_identical(resolve_freshness(query, fixture$con, NULL), connection)
})

test_that("table policies survive transformations and conflicting joins require an override", {
  fixture <- local_native_connection()
  local_lake_session(fixture$con)
  morning <- daily_at("07:00", "UTC")
  evening <- daily_at("19:00", "UTC")
  first <- lake_tbl("types", freshness = morning)
  second <- lake_tbl("types", freshness = evening)
  derived <- first |>
    dplyr::filter(id > 0) |>
    dplyr::mutate(double_id = id * 2) |>
    dplyr::group_by(id) |>
    dplyr::summarise(total = sum(double_id, na.rm = TRUE)) |>
    dplyr::select(id, total)
  expect_identical(query_freshness(derived), morning)
  expect_identical(query_freshness(dplyr::collapse(derived)), morning)
  expect_identical(query_freshness(dplyr::inner_join(first, first, by = "id")), morning)
  joined <- dplyr::inner_join(first, second, by = "id")
  expect_error(resolve_freshness(joined, fixture$con, NULL), "different freshness policies")
  collapsed <- dplyr::collapse(joined)
  expect_error(resolve_freshness(collapsed, fixture$con, NULL), "different freshness policies")
  expect_identical(resolve_freshness(collapsed, fixture$con, "always"), "always")
  expect_identical(resolve_freshness(joined, fixture$con, "always"), "always")
  expect_error(query_freshness(dplyr::union(first, second)), "different freshness policies")
  direct <- dplyr::tbl(fixture$con, "types")
  right_policy <- dplyr::inner_join(direct, first, by = "id")
  expect_identical(query_freshness(dplyr::collapse(right_policy)), morning)
})

test_that("connections retain only explicitly supplied freshness", {
  fixture <- local_native_connection()
  withr::local_options(ckdata.freshness = daily_at("07:00", "UTC"))
  explicit <- DBI::dbConnect(ckdata(), workgroup = "synthetic", role_arn = NULL, freshness = daily_at("09:00", "UTC"))
  withr::defer(DBI::dbDisconnect(explicit))
  expect_identical(explicit@state$freshness, daily_at("09:00", "UTC"))
  expect_null(fixture$con@state$freshness)
  withr::local_options(ckdata.freshness = "always")
  query <- dplyr::tbl(fixture$con, "types")
  expect_identical(resolve_freshness(query, fixture$con, NULL), "always")
  expect_identical(resolve_freshness(query, explicit, NULL), daily_at("09:00", "UTC"))
})

test_that("invalid schedule configuration fails without silently changing policies", {
  for (at in c("7:00", "24:00", "07:60", "07:00:00")) expect_error(daily_at(at, "UTC"), "HH:MM")
  expect_error(daily_at("07:00", "invented/invalid"), "timezone")
  expect_error(check_freshness(25), "freshness")
  withr::local_options(ckdata.freshness = NULL)
  withr::local_envvar(c(CKDATA_FRESHNESS_AT = "07:00", CKDATA_FRESHNESS_TZ = NA))
  expect_error(default_freshness(), "set both")
  withr::local_options(ckdata.freshness = "always")
  expect_identical(default_freshness(), "always")
})

test_that("daily expiry reruns the query after the next boundary", {
  fixture <- local_saved_tbl(data.frame(x = 1L))
  dir <- withr::local_tempdir()
  now <- as.POSIXct("2026-11-05 08:00:00", tz = "UTC")
  testthat::local_mocked_bindings(saved_now = function() now, .package = "ckdata")
  policy <- daily_at("07:00", "UTC")
  first <- collect_saved(fixture$query, freshness = policy, dir = dir)
  DBI::dbExecute(fixture$source, "INSERT INTO t VALUES (2)")
  now <- as.POSIXct("2026-11-06 06:59:59", tz = "UTC")
  expect_identical(collect_saved(fixture$query, freshness = policy, dir = dir), first)
  now <- as.POSIXct("2026-11-06 07:00:00", tz = "UTC")
  expect_identical(collect_saved(fixture$query, freshness = policy, dir = dir)$x, 1:2)
  expect_length(fixture$calls$queries, 2L)
})

test_that("cached revision checks refresh results only when the revision changes", {
  fixture <- local_saved_tbl(data.frame(x = 1L))
  checks <- 0L
  revision <- "build-a"
  policy <- freshness_check(function(con) {
    checks <<- checks + 1L
    revision
  })
  dir <- withr::local_tempdir()
  first <- collect_saved(fixture$query, freshness = policy, dir = dir)
  DBI::dbExecute(fixture$source, "INSERT INTO t VALUES (2)")
  expect_identical(collect_saved(fixture$query, freshness = policy, dir = dir), first)
  expect_identical(checks, 1L)
  policy$revisions$reset()
  expect_identical(collect_saved(fixture$query, freshness = policy, dir = dir), first)
  expect_identical(checks, 2L)
  revision <- "build-b"
  policy$revisions$reset()
  expect_identical(collect_saved(fixture$query, freshness = policy, dir = dir)$x, 1:2)
  expect_length(fixture$calls$queries, 2L)
  expect_identical(checks, 3L)
})

test_that("revision probe caching isolates contexts and can be disabled", {
  calls <- 0L
  policy <- freshness_check(function(con) {
    calls <<- calls + 1L
    "revision"
  })
  fixture <- local_native_connection()
  source_revision(policy, fixture$con, list(principal = "a"))
  source_revision(policy, fixture$con, list(principal = "b"))
  source_revision(policy, fixture$con, list(principal = "a"))
  expect_identical(calls, 2L)
  uncached <- freshness_check(policy$check, every = 0)
  source_revision(uncached, fixture$con, list(principal = "a"))
  source_revision(uncached, fixture$con, list(principal = "a"))
  expect_identical(calls, 4L)
})

test_that("revision probes expire automatically and are shared across queries on one source", {
  now <- as.POSIXct("2026-11-05 08:00:00", tz = "UTC")
  testthat::local_mocked_bindings(Sys.time = function() now, .package = "base")
  fixture <- local_saved_tbl(data.frame(x = 1L))
  checks <- 0L
  revision <- "build-a"
  policy <- freshness_check(function(con) {
    checks <<- checks + 1L
    revision
  }, every = 60)
  dir <- withr::local_tempdir()
  first <- collect_saved(fixture$query, freshness = policy, dir = dir)
  other <- dplyr::filter(fixture$query, x == 1L)
  expect_identical(collect_saved(other, freshness = policy, dir = dir), first)
  expect_identical(checks, 1L)
  DBI::dbExecute(fixture$source, "INSERT INTO t VALUES (2)")
  revision <- "build-b"
  now <- now + 59
  expect_identical(collect_saved(fixture$query, freshness = policy, dir = dir), first)
  expect_identical(checks, 1L)
  now <- now + 2
  expect_identical(collect_saved(fixture$query, freshness = policy, dir = dir)$x, 1:2)
  expect_identical(collect_saved(other, freshness = policy, dir = dir), first)
  expect_identical(checks, 2L)
  expect_length(fixture$calls$queries, 4L)
})

test_that("failed revision checks use stale data or warn and fetch without a revision", {
  fixture <- local_saved_tbl(data.frame(x = 1L))
  dir <- withr::local_tempdir()
  first <- collect_saved(fixture$query, freshness = function(con) "revision", dir = dir)
  check <- function(con) rlang::abort("synthetic metadata outage")
  expect_warning(stale <- collect_saved(fixture$query, freshness = check, dir = dir), class = "ckdata_saved_stale")
  expect_identical(stale, first)
  expect_length(fixture$calls$queries, 1L)
  expect_warning(fresh <- collect_saved(fixture$query, freshness = check, dir = withr::local_tempdir()), class = "ckdata_freshness_failed")
  expect_identical(fresh, first)
  expect_length(fixture$calls$queries, 2L)
  expect_warning(forced <- collect_saved(fixture$query, freshness = check, refresh = TRUE, dir = dir), class = "ckdata_freshness_failed")
  expect_identical(forced, first)
  expect_length(fixture$calls$queries, 3L)
})
