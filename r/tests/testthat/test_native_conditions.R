test_that("native errors retain structured cancellation fields", {
  testthat::local_mocked_bindings(
    native_job_value = function(job) {
      list(
        success = FALSE,
        error = list(
          message = "synthetic failure",
          cancellation = "unconfirmed",
          remote_may_be_running = TRUE
        )
      )
    },
    .package = "ckdata"
  )
  cnd <- rlang::catch_cnd(native_value(NULL), classes = "error")
  expect_s3_class(cnd, "ckdata_native_error")
  expect_identical(cnd$cancellation, "unconfirmed")
  expect_true(cnd$remote_may_be_running)
  expect_match(rlang::cnd_message(cnd), "synthetic failure", fixed = TRUE)
})

test_that("native SSO metadata produces a typed condition for connection recovery", {
  target <- list(profile = "synthetic", session = "fixture-session")
  testthat::local_mocked_bindings(
    native_job_value = function(job) {
      list(
        success = FALSE,
        error = list(message = "AWS SSO sign-in required", sso_login = target)
      )
    },
    .package = "ckdata"
  )
  condition <- rlang::catch_cnd(native_value(NULL), classes = "error")
  expect_s3_class(condition, "ckdata_sso_login_required")
  expect_s3_class(condition, "ckdata_native_error")
  expect_identical(condition$sso_login, target)
})

test_that("cancellation cleanup uses structured status rather than English messages", {
  testthat::local_mocked_bindings(
    native_cancel = function(job) NULL,
    native_ready = function(job) TRUE,
    .package = "ckdata"
  )
  testthat::local_mocked_bindings(
    native_job_value = function(job) {
      list(
        success = FALSE,
        error = list(
          message = "query may be active",
          cancellation = "unconfirmed",
          remote_may_be_running = TRUE
        )
      )
    },
    .package = "ckdata"
  )
  expect_warning(
    drain_native_job(NULL),
    "query may be active",
    class = "ckdata_cancellation_warning"
  )
  withr::local_options(warn = 2)
  expect_message(drain_native_job(NULL), "query may be active")
  testthat::local_mocked_bindings(
    native_job_value = function(job) {
      list(
        success = FALSE,
        error = list(
          message = "unconfirmed and outcome unknown are only text",
          cancellation = NULL,
          remote_may_be_running = FALSE
        )
      )
    },
    .package = "ckdata"
  )
  expect_no_warning(drain_native_job(NULL))
})

test_that("native handle errors become rlang conditions with their cause", {
  cnd <- rlang::catch_cnd(native_value(NULL), classes = "error")
  expect_s3_class(cnd, "ckdata_native_error")
  expect_s3_class(cnd$parent, "error")
  expect_identical(
    conditionMessage(cnd$parent),
    "invalid ckdata native handle"
  )
  expect_gt(nrow(cnd$trace), 0L)
})

test_that("a cancelled native job signals an error with a trace", {
  job <- native_probe_job(60000)
  native_cancel(job)
  cnd <- rlang::catch_cnd(native_wait(job), classes = "error")
  expect_s3_class(cnd, "ckdata_native_error")
  expect_gt(nrow(cnd$trace), 0L)
  expect_false(cnd$remote_may_be_running)
})
