test_that("an installed package decodes results in a fresh R session", {
  package <- system.file(package = "ckdata")
  if (!file.exists(file.path(package, "Meta", "package.rds"))) {
    skip("Fresh-session installation check requires an installed package")
  }
  script <- tempfile(fileext = ".R")
  withr::defer(unlink(script))
  writeLines(
    c(
      paste0("library(ckdata, lib.loc = ", deparse(dirname(package)), ")"),
      "ns <- rlang::ns_env('ckdata')",
      "result <- rlang::env_get(ns, 'native_probe_result')(FALSE)",
      "stream <- rlang::env_get(ns, 'native_result_fetch')(result, -1)",
      "frame <- rlang::env_get(ns, 'stream_frame')(stream)",
      "testthat::expect_equal(nrow(frame), 2L)",
      "testthat::expect_s3_class(frame$big, 'integer64')"
    ),
    script
  )
  output <- suppressWarnings(system2(
    file.path(R.home("bin"), "Rscript"),
    c("--vanilla", shQuote(script)),
    stdout = TRUE,
    stderr = TRUE
  ))
  expect_null(attr(output, "status"), info = paste(output, collapse = "\n"))
})

test_that("unhandled native failures render once with a wrapped console callback", {
  package <- system.file(package = "ckdata")
  if (!file.exists(file.path(package, "Meta", "package.rds"))) {
    skip("Console rendering check requires an installed package")
  }
  script <- tempfile(fileext = ".R")
  withr::defer(unlink(script))
  writeLines(
    c(
      paste0("library(ckdata, lib.loc = ", deparse(dirname(package)), ")"),
      r"(
      Sys.unsetenv("TESTTHAT")
      rlang::global_entrace()
      previous_error <- getOption("error")
      options(error = function() {
        if (!is.null(previous_error)) eval(previous_error)
      })
      job <- ckdata:::native_probe_job(60000)
      ckdata:::native_cancel(job)
      ckdata:::native_wait(job)
      stopifnot(inherits(rlang::last_error(), "ckdata_native_error"))
      stopifnot(nrow(rlang::last_error()$trace) > 0L)
      stopifnot(!is.null(rlang::last_trace()))
      cat("CANCEL_TRACE_RETAINED\n")
      ckdata:::native_value(NULL)
      stopifnot(inherits(rlang::last_error(), "ckdata_native_error"))
      stopifnot(nrow(rlang::last_error()$trace) > 0L)
      cat("HANDLE_TRACE_RETAINED\n")
      q("no")
    )"
    ),
    script
  )
  output <- suppressWarnings(system2(
    file.path(R.home("bin"), "R"),
    c("--vanilla", "--quiet", "--interactive", "--no-readline"),
    stdin = script,
    stdout = TRUE,
    stderr = TRUE,
    timeout = 30
  ))
  expect_null(attr(output, "status"), info = paste(output, collapse = "\n"))
  expect_equal(sum(grepl("^! synthetic query cancelled$", output)), 1L)
  expect_equal(sum(grepl("^! invalid ckdata native handle$", output)), 1L)
  expect_equal(
    sum(grepl("Run `rlang::last_trace()`", output, fixed = TRUE)),
    2L
  )
  expect_true("CANCEL_TRACE_RETAINED" %in% output)
  expect_true("HANDLE_TRACE_RETAINED" %in% output)
})

test_that("forked sessions reject inherited native runtimes promptly", {
  skip_on_os("windows")
  package <- system.file(package = "ckdata")
  if (!file.exists(file.path(package, "Meta", "package.rds"))) {
    skip("Forked-session check requires an installed package")
  }
  script <- tempfile(fileext = ".R")
  withr::defer(unlink(script))
  writeLines(
    c(
      paste0("library(ckdata, lib.loc = ", deparse(dirname(package)), ")"),
      r"(
      ns <- rlang::ns_env("ckdata")
      probe <- rlang::env_get(ns, "native_probe_job")
      wait <- rlang::env_get(ns, "native_wait")
      info <- rlang::env_get(ns, "native_result_info")
      clear <- rlang::env_get(ns, "native_result_clear")
      shutdown <- rlang::env_get(ns, "native_shutdown")

      check_job <- function() {
        value <- wait(probe(0))
        stopifnot(info(value$handle)$rows == 2)
        clear(value$handle)
        TRUE
      }

      collect_child <- function(child) {
        collected <- FALSE
        withr::defer({
          if (!collected) {
            tools::pskill(child$pid, tools::SIGKILL)
            parallel::mccollect(child)
          }
        })
        deadline <- proc.time()[["elapsed"]] + 5
        repeat {
          result <- parallel::mccollect(child, wait = FALSE)
          if (!rlang::is_null(result)) {
            collected <- TRUE
            stopifnot(identical(unname(result), list(TRUE)))
            return(invisible(NULL))
          }
          if (proc.time()[["elapsed"]] >= deadline) {
            stop("forked native calls did not finish within five seconds")
          }
          Sys.sleep(0.01)
        }
      }

      # Loading or shutting down an unused package does not initialize a worker.
      shutdown()
      collect_child(parallel::mcparallel({
        check_job()
        shutdown()
        TRUE
      }, mc.set.seed = FALSE, silent = TRUE))

      check_job()
      # Keep a synthetic task pending so an unguarded child shutdown would hang.
      pending <- probe(60000)
      collect_child(parallel::mcparallel({
        message <- tryCatch(probe(0), error = conditionMessage)
        stopifnot(is.character(message), length(message) == 1L)
        stopifnot(grepl("forked process", message, fixed = TRUE))
        stopifnot(grepl("fresh R session or PSOCK", message, fixed = TRUE))
        message <- tryCatch(shutdown(), error = conditionMessage)
        stopifnot(is.character(message), length(message) == 1L)
        stopifnot(grepl("forked process", message, fixed = TRUE))
        TRUE
      }, mc.set.seed = FALSE, silent = TRUE))
      check_job()
      shutdown()
      message <- tryCatch(wait(pending), error = conditionMessage)
      stopifnot(is.character(message), length(message) == 1L)
      stopifnot(grepl("synthetic query cancelled", message, fixed = TRUE))
    )"
    ),
    script
  )
  output <- suppressWarnings(system2(
    file.path(R.home("bin"), "Rscript"),
    c("--vanilla", shQuote(script)),
    stdout = TRUE,
    stderr = TRUE,
    timeout = 30
  ))
  expect_null(attr(output, "status"), info = paste(output, collapse = "\n"))
})
