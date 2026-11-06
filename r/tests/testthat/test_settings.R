test_that("the managed connection preserves environment compatibility without staging settings", {
  fixture <- local_native_connection()
  rlang::local_bindings(con = NULL, .env = lake_state)
  withr::local_envvar(
    CK_DATALAKE_WORKGROUP = "synthetic",
    CK_DATALAKE_INTERNAL_PROFILE = "synthetic-profile", CK_DATALAKE_REGION = "eu-west-1",
    CK_DATALAKE_S3_STAGING_DIR = NA, CK_DATALAKE_ROLE_ARN = NA
  )
  withr::local_options(ckdata.workgroup = NULL)
  con <- lake_con()
  withr::defer(DBI::dbDisconnect(con))
  expect_identical(lake_con(), con)
  args <- tail(fixture$calls$connections, 1)[[1]]
  expect_identical(args$profile, "synthetic-profile")
  expect_identical(args$region, "eu-west-1")
  expect_false("s3_staging_dir" %in% names(args))
})

test_that("a serialized result is invalid even if its R state still says open", {
  con <- local_native_connection()$con
  result <- DBI::dbSendQuery(con, "SELECT synthetic")
  restored <- unserialize(serialize(result, NULL))
  expect_false(DBI::dbIsValid(restored))
  DBI::dbClearResult(result)
})

test_that("an expired managed connection is replaced only after an explicit disconnect", {
  fixture <- local_native_connection()
  rlang::local_bindings(con = fixture$con, .env = lake_state)
  withr::local_options(ckdata.workgroup = "synthetic")
  withr::local_envvar(CK_DATALAKE_ROLE_ARN = NA)
  fixture$calls$session_error <- "assumed-role session expired"
  expect_false(DBI::dbIsValid(fixture$con))
  expect_identical(lake_con(), fixture$con)
  expect_error(lake_query("SELECT 1"), "session expired")
  expect_length(fixture$calls$queries, 0L)
  expect_length(fixture$calls$connections, 1L)
  expect_true(lake_disconnect())
  expect_false(fixture$con@state$open)
  expect_true(lake_disconnect())
  fixture$calls$session_error <- ""
  replacement <- lake_con()
  withr::defer(lake_disconnect())
  expect_false(rlang::is_reference(replacement@state, fixture$con@state))
  expect_length(fixture$calls$connections, 2L)
  expect_true(DBI::dbIsValid(replacement))
})

test_that("disconnecting an unused managed session never opens a connection", {
  fixture <- local_native_connection()
  rlang::local_bindings(con = NULL, .env = lake_state)
  expect_true(lake_disconnect())
  expect_length(fixture$calls$connections, 1L)
})
