# Mock only the AWS-facing calls. Query waits and result cursors are native:
# this fixture exercises the R/Arrow boundary without AWS credentials.
local_native_connection <- function(env = rlang::caller_env()) {
  calls <- rlang::new_environment()
  calls$queries <- character()
  calls$parameters <- list()
  calls$schema_only <- logical()
  calls$metadata <- 0L
  calls$metadata_args <- list()
  calls$account <- "000000000000"
  calls$connections <- list()
  info <- function(object) {
    list(
      open = TRUE,
      account = calls$account,
      arn = "arn:aws:iam::000000000000:role/synthetic",
      region = "us-west-2",
      workgroup = "synthetic",
      schema = "fixture"
    )
  }
  original_wait <- native_wait
  testthat::local_mocked_bindings(
    native_connect = function(settings, login_allowed = FALSE) {
      calls$connections <- c(
        calls$connections,
        list(c(settings, list(login = login_allowed)))
      )
      structure(list(value = NULL), class = "metadata_fixture")
    },
    native_connection_info = info,
    native_query = function(object, sql, timeout, parameters, schema_only) {
      calls$queries <- c(calls$queries, sql)
      calls$parameters <- c(calls$parameters, list(parameters))
      calls$schema_only <- c(calls$schema_only, schema_only)
      if (schema_only) {
        structure(
          list(
            value = list(
              handle = native_probe_result(TRUE),
              query_id = "synthetic-empty"
            )
          ),
          class = "metadata_fixture"
        )
      } else {
        native_probe_job(0)
      }
    },
    native_disconnect = function(object) invisible(NULL),
    native_metadata = function(object, schema, table) {
      calls$metadata <- calls$metadata + 1L
      calls$metadata_args <- c(
        calls$metadata_args,
        list(list(schema = schema, table = table))
      )
      value <- if (rlang::is_null(table)) {
        "types"
      } else {
        native_probe_metadata()
      }
      structure(list(value = value), class = "metadata_fixture")
    },
    native_wait = function(job) {
      if (inherits(job, "metadata_fixture")) job$value else original_wait(job)
    },
    .package = "ckdata",
    .env = env
  )
  con <- DBI::dbConnect(
    ckdata(),
    workgroup = "synthetic",
    schema = "fixture",
    role_arn = NULL
  )
  withr::defer(
    {
      if (rlang::is_true(con@state$open)) {
        suppressWarnings(DBI::dbDisconnect(con))
      }
    },
    envir = env
  )
  list(con = con, calls = calls)
}

# Observe cleanup while letting the real native job cancel and finish.
local_native_cancellation <- function(env = rlang::caller_env()) {
  calls <- rlang::env(cancelled = list(), terminal_error = NULL)
  cancel <- native_cancel
  value <- native_value
  testthat::local_mocked_bindings(
    native_cancel = function(job) {
      calls$cancelled <- c(calls$cancelled, list(job))
      cancel(job)
    },
    native_value = function(job) {
      rlang::try_fetch(value(job), error = function(cnd) {
        calls$terminal_error <- cnd
        rlang::cnd_signal(cnd)
      })
    },
    .package = "ckdata",
    .env = env
  )
  calls
}
