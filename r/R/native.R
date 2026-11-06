native_value <- function(job) {
  outcome <- rlang::try_fetch(
    native_job_value(job),
    error = function(cnd) {
      rlang::abort(rlang::cnd_message(cnd), class = "ckdata_native_error", parent = cnd)
    }
  )
  if (rlang::is_true(outcome$success)) {
    return(outcome$value)
  }
  error <- outcome$error
  rlang::abort(
    error$message,
    class = "ckdata_native_error",
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
        while (!native_ready(job)) Sys.sleep(0.05)
        native_value(job)
      },
      classes = "error"
    )
    if (rlang::is_true(outcome$remote_may_be_running)) {
      detail <- rlang::cnd_message(outcome)
      rlang::try_fetch(
        rlang::warn(detail, class = "ckdata_cancellation_warning", cancellation = outcome$cancellation),
        error = function(cnd) rlang::inform(detail)
      )
    }
  })
}

native_wait <- function(job) {
  completed <- FALSE
  withr::defer(if (!completed) drain_native_job(job))
  while (!native_ready(job)) Sys.sleep(0.05)
  completed <- TRUE
  native_value(job)
}
