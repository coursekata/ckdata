local_sso_failure <- function(failures = 1L, env = rlang::caller_env()) {
  fixture <- local_native_connection(env)
  connect <- native_connect
  calls <- rlang::env(attempts = 0L, logins = 0L)
  target <- list(profile = "synthetic-workforce", session = "synthetic-session")
  testthat::local_mocked_bindings(
    native_connect = function(...) {
      calls$attempts <- calls$attempts + 1L
      if (calls$attempts <= failures) {
        rlang::abort("SSO sign-in required", class = c("ckdata_sso_login_required", "ckdata_native_error"), sso_login = target)
      }
      connect(...)
    },
    aws_sso_login = function(actual) {
      expect_identical(actual, target)
      expect_length(fixture$calls$queries, 0L)
      calls$logins <- calls$logins + 1L
    },
    .package = "ckdata", .env = env
  )
  list(calls = calls, fixture = fixture)
}

test_that("interactive first use signs in once and retains the managed connection", {
  withr::local_options(ckdata.login = NULL, ckdata.workgroup = "synthetic")
  testthat::local_mocked_bindings(is_interactive = function() TRUE, .package = "rlang")
  rlang::local_bindings(con = NULL, tables = lake_state$tables[0, ], .env = lake_state)
  context <- local_sso_failure()
  withr::defer(lake_disconnect())
  lake_tbl("types")
  lake_tbl("types")
  expect_identical(context$calls$attempts, 2L)
  expect_identical(context$calls$logins, 1L)
  expect_length(context$fixture$calls$queries, 0L)
})

test_that("connection and application switches disable sign-in and scripts default to disabled", {
  context <- local_sso_failure(Inf)
  withr::local_options(ckdata.login = NULL, ckdata.workgroup = "synthetic")
  testthat::local_mocked_bindings(is_interactive = function() FALSE, .package = "rlang")
  expect_error(DBI::dbConnect(ckdata()), "automatic sign-in is disabled", class = "ckdata_sso_login_required")
  expect_error(DBI::dbConnect(ckdata(), login = FALSE), class = "ckdata_sso_login_required")
  withr::local_options(ckdata.login = FALSE)
  testthat::local_mocked_bindings(is_interactive = function() TRUE, .package = "rlang")
  rlang::local_bindings(con = NULL, .env = lake_state)
  expect_error(lake_tbl("types"), class = "ckdata_sso_login_required")
  expect_identical(context$calls$attempts, 3L)
  expect_identical(context$calls$logins, 0L)
  expect_error(DBI::dbConnect(ckdata(), login = "never"), "must be", class = "rlang_error")
  expect_identical(context$calls$attempts, 3L)
  expect_error(DBI::dbConnect(ckdata(), login = TRUE), class = "ckdata_sso_login_required")
  expect_identical(context$calls$attempts, 5L)
  expect_identical(context$calls$logins, 1L)
})

test_that("a second SSO failure propagates without another browser or retry", {
  context <- local_sso_failure(Inf)
  expect_error(DBI::dbConnect(ckdata(), workgroup = "synthetic", login = TRUE), class = "ckdata_sso_login_required")
  expect_identical(context$calls$attempts, 2L)
  expect_identical(context$calls$logins, 1L)
})

test_that("valid credentials and unrelated connection failures never start sign-in", {
  fixture <- local_native_connection()
  testthat::local_mocked_bindings(aws_sso_login = function(...) rlang::abort("unexpected login"), .package = "ckdata")
  con <- DBI::dbConnect(ckdata(), workgroup = "synthetic", login = TRUE)
  withr::defer(DBI::dbDisconnect(con))
  testthat::local_mocked_bindings(native_connect = function(...) rlang::abort("AccessDenied", class = "ckdata_native_error"), .package = "ckdata")
  expect_error(DBI::dbConnect(ckdata(), workgroup = "synthetic", login = TRUE), "AccessDenied")
  expect_length(fixture$calls$queries, 0L)
})

test_that("failed or interrupted login never retries the connection", {
  context <- local_sso_failure()
  testthat::local_mocked_bindings(aws_sso_login = function(...) rlang::abort("cancelled", class = "ckdata_login_failed"), .package = "ckdata")
  expect_error(DBI::dbConnect(ckdata(), workgroup = "synthetic", login = TRUE), class = "ckdata_login_failed")
  expect_identical(context$calls$attempts, 1L)
  testthat::local_mocked_bindings(aws_sso_login = function(...) rlang::interrupt(), .package = "ckdata")
  context$calls$attempts <- 0L
  expect_s3_class(rlang::catch_cnd(DBI::dbConnect(ckdata(), workgroup = "synthetic", login = TRUE), classes = "interrupt"), "interrupt")
  expect_identical(context$calls$attempts, 1L)
})

test_that("AWS CLI receives the SSO source profile with quoted arguments", {
  testthat::local_mocked_bindings(Sys.which = function(...) c(aws = "/synthetic/aws"), .package = "base")
  calls <- list()
  testthat::local_mocked_bindings(system2 = function(command, args, ...) {
    if (identical(args, "--version")) {
      return("aws-cli/2.39.2 synthetic")
    }
    calls[[length(calls) + 1L]] <<- list(command = command, args = args)
    0L
  }, .package = "base")
  target <- list(profile = "workforce name; literal", session = "SSO session")
  expect_message(aws_sso_login(target), "Opening AWS SSO")
  expect_identical(unname(calls[[1]]$command), "/synthetic/aws")
  expect_identical(calls[[1]]$args, c("sso", "login", "--profile", shQuote(target$profile), "--sso-session", shQuote(target$session), "--no-cli-pager", "--no-cli-auto-prompt"))
  target$session <- NULL
  expect_message(aws_sso_login(target), "Opening AWS SSO")
  expect_false("--sso-session" %in% calls[[2]]$args)
})

test_that("missing AWS CLI and unsuccessful login produce actionable errors", {
  target <- list(profile = "synthetic", session = NULL)
  testthat::local_mocked_bindings(Sys.which = function(...) c(aws = ""), .package = "base")
  expect_error(aws_sso_login(target), "AWS CLI v2", class = "ckdata_login_tool_missing")
  testthat::local_mocked_bindings(Sys.which = function(...) c(aws = "/synthetic/aws"), system2 = function(...) "aws-cli/1.0 synthetic", .package = "base")
  expect_error(aws_sso_login(target), "AWS CLI v2", class = "ckdata_login_tool_version")
  testthat::local_mocked_bindings(system2 = function(command, args, ...) {
    if (identical(args, "--version")) "aws-cli/2.39.2 synthetic" else 130L
  }, .package = "base")
  expect_message(expect_error(aws_sso_login(target), class = "ckdata_login_failed"), "Opening AWS SSO")
})
