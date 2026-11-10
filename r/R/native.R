native_value <- function(job) {
  outcome <- rlang::try_fetch(
    native_job_value(job),
    error = function(cnd) {
      rlang::abort(
        "Could not retrieve the native result.",
        class = "ckdata_native_error",
        parent = cnd
      )
    }
  )
  if (rlang::is_true(outcome$success)) {
    return(outcome$value)
  }
  error <- outcome$error
  credentials_unavailable <- rlang::is_true(error$credentials_unavailable)
  rlang::abort(
    c(
      error$message,
      if (rlang::is_true(error$login_disabled)) {
        c(
          i = "Automatic SSO sign-in is disabled; reconnect with `login = TRUE`, or sign in using your preferred AWS method."
        )
      },
      if (credentials_unavailable) {
        c(
          i = "Check the AWS profile configuration. Start a new R session after changing startup settings."
        )
      }
    ),
    class = c(
      if (!rlang::is_null(error$sso_login)) "ckdata_sso_login_required",
      if (credentials_unavailable) "ckdata_credentials_unavailable",
      "ckdata_native_error"
    ),
    sso_login = error$sso_login,
    cancellation = error$cancellation,
    remote_may_be_running = error$remote_may_be_running
  )
}

drain_native_job <- function(job) {
  # Cleanup must preserve the original interrupt or time-limit condition.
  suspendInterrupts({
    outcome <- rlang::catch_cnd(
      {
        native_cancel(job)
        while (!native_ready(job)) {
          Sys.sleep(0.05)
        }
        native_value(job)
      },
      classes = "error"
    )
    if (rlang::is_true(outcome$remote_may_be_running)) {
      detail <- rlang::cnd_message(outcome)
      rlang::try_fetch(
        rlang::warn(
          detail,
          class = "ckdata_cancellation_warning",
          cancellation = outcome$cancellation
        ),
        error = function(cnd) rlang::inform(detail)
      )
    }
  })
}

native_wait <- function(job) {
  completed <- FALSE
  withr::defer(if (!completed) drain_native_job(job))
  while (!native_ready(job)) {
    target <- native_login_target(job)
    if (!rlang::is_null(target)) {
      rlang::try_fetch(
        {
          success <- aws_sso_login(target, job = job)
          native_login_complete(job, !identical(success, FALSE))
        },
        error = function(cnd) {
          native_login_complete(job, FALSE)
          rlang::cnd_signal(cnd)
        },
        interrupt = function(cnd) {
          native_login_complete(job, FALSE)
          rlang::cnd_signal(cnd)
        }
      )
    }
    Sys.sleep(0.05)
  }
  completed <- TRUE
  native_value(job)
}
