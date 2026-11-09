test_that("explicit profile authentication ignores SSO environment values", {
  withr::local_envvar(c(
    CKDATA_PROFILE = "environment-profile",
    CKDATA_SSO_ACCOUNT_ID = "111122223333",
    CKDATA_SSO_ROLE_NAME = "EnvironmentReader"
  ))
  fixture <- local_native_connection()
  con <- DBI::dbConnect(
    ckdata(),
    workgroup = "synthetic",
    profile = "argument-profile"
  )
  withr::defer(DBI::dbDisconnect(con))
  selected <- tail(fixture$calls$connections, 1)[[1]]
  expect_identical(selected$profile, "argument-profile")
  expect_length(selected$sso, 0)
  con2 <- DBI::dbConnect(
    ckdata(),
    workgroup = "synthetic",
    profile = "argument-profile",
    sso_role_name = "ArgumentReader"
  )
  withr::defer(DBI::dbDisconnect(con2))
  selected <- tail(fixture$calls$connections, 1)[[1]]
  expect_identical(selected$sso, list(role_name = "ArgumentReader"))
})

test_that("direct SSO merges arguments over environment and keeps profiles as defaults only", {
  withr::local_envvar(c(
    CKDATA_PROFILE = "fallback-profile",
    CKDATA_SSO_START_URL = "https://synthetic.awsapps.com/start",
    CKDATA_SSO_REGION = "us-west-1",
    CKDATA_SSO_ACCOUNT_ID = "000000000000",
    CKDATA_SSO_ROLE_NAME = "EnvironmentReader",
    CKDATA_SSO_SESSION = NA,
    CKDATA_SSO_REGISTRATION_SCOPES = NA
  ))
  fixture <- local_native_connection()
  con <- DBI::dbConnect(
    ckdata(),
    workgroup = "synthetic",
    sso_role_name = "ArgumentReader"
  )
  withr::defer(DBI::dbDisconnect(con))
  selected <- tail(fixture$calls$connections, 1)[[1]]
  expect_null(selected$profile)
  expect_identical(
    selected$sso,
    list(
      start_url = "https://synthetic.awsapps.com/start",
      region = "us-west-1",
      account_id = "000000000000",
      role_name = "ArgumentReader",
      fallback_profile = "fallback-profile"
    )
  )
  con2 <- DBI::dbConnect(
    ckdata(),
    workgroup = "synthetic",
    profile = NULL,
    sso_role_name = NULL
  )
  withr::defer(DBI::dbDisconnect(con2))
  selected <- tail(fixture$calls$connections, 1)[[1]]
  expect_null(selected$sso$fallback_profile)
  expect_null(selected$sso$role_name)
  expect_error(DBI::dbConnect(ckdata(), workgroup = "synthetic", profile = ""))
})

test_that("login argument overrides the option and unattended sessions default to disabled", {
  fixture <- local_native_connection()
  withr::local_options(ckdata.login = NULL)
  testthat::local_mocked_bindings(
    is_interactive = function() FALSE,
    .package = "rlang"
  )
  con <- DBI::dbConnect(ckdata(), workgroup = "synthetic")
  withr::defer(DBI::dbDisconnect(con))
  expect_false(tail(fixture$calls$connections, 1)[[1]]$login)
  withr::local_options(ckdata.login = TRUE)
  con2 <- DBI::dbConnect(ckdata(), workgroup = "synthetic", login = FALSE)
  withr::defer(DBI::dbDisconnect(con2))
  expect_false(tail(fixture$calls$connections, 1)[[1]]$login)
  withr::local_options(ckdata.login = FALSE)
  con3 <- DBI::dbConnect(ckdata(), workgroup = "synthetic", login = TRUE)
  withr::defer(DBI::dbDisconnect(con3))
  expect_true(tail(fixture$calls$connections, 1)[[1]]$login)
  expect_error(DBI::dbConnect(
    ckdata(),
    workgroup = "synthetic",
    login = "never"
  ))
})

local_login_job <- function(login_failure = NULL, env = rlang::caller_env()) {
  calls <- rlang::env(
    ready = FALSE,
    logins = 0L,
    completions = logical(),
    values = 0L
  )
  testthat::local_mocked_bindings(
    native_ready = function(job) calls$ready,
    native_login_target = function(job) {
      list(profile = "synthetic", session = "invented")
    },
    aws_sso_login = function(target, job = NULL) {
      calls$logins <- calls$logins + 1L
      if (!rlang::is_null(login_failure)) {
        login_failure()
      }
      invisible(TRUE)
    },
    native_login_complete = function(job, success) {
      calls$completions <- c(calls$completions, success)
      calls$ready <- TRUE
    },
    native_job_value = function(job) {
      calls$values <- calls$values + 1L
      list(success = TRUE, value = "same-query-result")
    },
    native_cancel = function(job) {
      calls$ready <- TRUE
    },
    .package = "ckdata",
    .env = env
  )
  calls
}

test_that("waiting for sign-in resumes the same native job without resubmitting SQL", {
  calls <- local_login_job()
  expect_identical(native_wait("same-job"), "same-query-result")
  expect_identical(calls$logins, 1L)
  expect_identical(calls$completions, TRUE)
  expect_identical(calls$values, 1L)
})

test_that("failed and interrupted sign-in notify the waiting provider and preserve the condition", {
  calls <- local_login_job(function() {
    rlang::abort("cancelled", class = "ckdata_login_failed")
  })
  expect_error(native_wait("same-job"), class = "ckdata_login_failed")
  expect_identical(calls$logins, 1L)
  expect_identical(calls$completions, FALSE)
  calls <- local_login_job(function() rlang::interrupt())
  expect_s3_class(
    rlang::catch_cnd(native_wait("same-job"), classes = "interrupt"),
    "interrupt"
  )
  expect_identical(calls$completions, FALSE)
})

test_that("a credential failure stops managed table discovery with one error", {
  withr::local_options(ckdata.workgroup = "synthetic", ckdata.login = TRUE)
  fixture <- local_native_connection()
  rlang::local_bindings(con = NULL, .env = lake_state)
  calls <- rlang::env(connections = 0L, values = 0L, errors = 0L)
  testthat::local_mocked_bindings(
    native_connect = function(...) {
      calls$connections <- calls$connections + 1L
      "synthetic-failed-job"
    },
    native_ready = function(job) TRUE,
    native_job_value = function(job) {
      calls$values <- calls$values + 1L
      list(
        success = FALSE,
        error = list(
          message = "AWS credentials could not be loaded",
          credentials_unavailable = TRUE
        )
      )
    },
    native_cancel = function(...) rlang::abort("unexpected cleanup"),
    aws_sso_login = function(...) rlang::abort("unexpected login"),
    .package = "ckdata"
  )
  cnd <- rlang::catch_cnd(
    withCallingHandlers(lake_tbl("types"), error = function(cnd) {
      calls$errors <- calls$errors + 1L
    }),
    classes = "error"
  )
  expect_s3_class(cnd, "ckdata_credentials_unavailable")
  expect_match(rlang::cnd_message(cnd), "Start a new R session", fixed = TRUE)
  expect_identical(calls$connections, 1L)
  expect_identical(calls$values, 1L)
  expect_identical(calls$errors, 1L)
  expect_null(lake_state$con)
  expect_identical(fixture$calls$metadata, 0L)
  expect_length(fixture$calls$queries, 0L)
})


test_that("AWS CLI receives literal arguments and a private subprocess-only SSO configuration", {
  testthat::local_mocked_bindings(
    aws_cli = function() "/synthetic/aws",
    .package = "ckdata"
  )
  target <- list(
    profile = "workforce name; literal",
    session = "SSO session",
    config = "[sso-session SSO session]\nsso_region = us-west-1"
  )
  original <- Sys.getenv("AWS_CONFIG_FILE", unset = NA_character_)
  observed <- NULL
  testthat::local_mocked_bindings(
    run_aws_cli = function(aws, args, env, job) {
      expect_identical(aws, "/synthetic/aws")
      expect_identical(
        args,
        c(
          "sso",
          "login",
          "--profile",
          target$profile,
          "--sso-session",
          target$session,
          "--no-cli-pager",
          "--no-cli-auto-prompt"
        )
      )
      expect_identical(
        Sys.getenv("AWS_CONFIG_FILE", unset = NA_character_),
        original
      )
      observed <<- unname(env[["AWS_CONFIG_FILE"]])
      expect_identical(
        paste(readLines(observed), collapse = "\n"),
        target$config
      )
      0L
    },
    .package = "ckdata"
  )
  expect_message(aws_sso_login(target), "Opening AWS SSO")
  expect_false(file.exists(observed))
  expect_identical(
    Sys.getenv("AWS_CONFIG_FILE", unset = NA_character_),
    original
  )
})

test_that("CLI absence, unsupported version and failed exit have actionable errors", {
  target <- list(profile = "synthetic", session = NULL)
  testthat::local_mocked_bindings(
    Sys.which = function(...) c(aws = ""),
    .package = "base"
  )
  expect_error(
    aws_sso_login(target),
    "AWS CLI v2",
    class = "ckdata_login_tool_missing"
  )
  testthat::local_mocked_bindings(
    Sys.which = function(...) c(aws = "/synthetic/aws"),
    system2 = function(...) "aws-cli/1.0 synthetic",
    .package = "base"
  )
  expect_error(aws_sso_login(target), class = "ckdata_login_tool_version")
  testthat::local_mocked_bindings(
    aws_cli = function() "/synthetic/aws",
    run_aws_cli = function(...) 130L,
    .package = "ckdata"
  )
  expect_message(
    expect_error(aws_sso_login(target), class = "ckdata_login_failed"),
    "Opening AWS SSO"
  )
})

test_that("a finished or timed-out native job stops the login child process", {
  calls <- rlang::env(killed = FALSE)
  testthat::local_mocked_bindings(
    start_aws_cli = function(...) {
      list(is_alive = function() TRUE, kill_tree = function() {
        calls$killed <- TRUE
      })
    },
    native_ready = function(job) TRUE,
    .package = "ckdata"
  )
  expect_identical(
    run_aws_cli("unused", character(), NULL, job = "finished"),
    NA_integer_
  )
  expect_true(calls$killed)
})

test_that("disabled renewal keeps sign-in guidance without launching a browser", {
  testthat::local_mocked_bindings(
    native_job_value = function(job) {
      list(
        success = FALSE,
        error = list(
          message = "AWS credentials require sign-in",
          sso_login = list(profile = "synthetic"),
          login_disabled = TRUE
        )
      )
    },
    aws_sso_login = function(...) rlang::abort("unexpected login"),
    .package = "ckdata"
  )
  expect_error(
    native_value("synthetic"),
    "Automatic SSO sign-in is disabled",
    class = "ckdata_sso_login_required"
  )
})

test_that("the login subprocess inherits ordinary environment values without changing its parent", {
  withr::local_envvar(CKDATA_SYNTHETIC_CHILD = "parent")
  script <- 'quit(status = if (identical(Sys.getenv("CKDATA_SYNTHETIC_CHILD"), "child")) 0L else 1L)'
  executable <- file.path(
    R.home("bin"),
    if (.Platform$OS.type == "windows") "Rscript.exe" else "Rscript"
  )
  expect_identical(
    run_aws_cli(
      executable,
      c("--vanilla", "-e", script),
      c("current", CKDATA_SYNTHETIC_CHILD = "child")
    ),
    0L
  )
  expect_identical(Sys.getenv("CKDATA_SYNTHETIC_CHILD"), "parent")
})
