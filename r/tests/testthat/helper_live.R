# Relative equality on epoch-sized numbers can hide a lost fractional second.
expect_timestamp_equal <- function(actual, expected, tolerance = 1e-4) {
  testthat::expect_s3_class(actual, "POSIXct")
  testthat::expect_identical(is.na(actual), is.na(expected))
  difference <- abs(as.numeric(actual) - as.numeric(expected))
  testthat::expect_lte(max(c(0, difference), na.rm = TRUE), tolerance)
}

validate_live_deployment <- function(deployment, seat_name) {
  if (!rlang::is_list(deployment)) {
    cli::cli_abort("{.arg deployment} must be a deployment object.", class = "ckdata_acceptance_deployment")
  }
  if (!rlang::is_string(seat_name) || !seat_name %in% c("internal", "research")) {
    cli::cli_abort("{.arg seat_name} must be {.val internal} or {.val research}.", class = "ckdata_acceptance_deployment")
  }
  account <- deployment$account_id
  if (!rlang::is_string(account) || !stringr::str_detect(account, "^[0-9]{12}$")) {
    cli::cli_abort("{.code deployment$account_id} must be a 12-digit account ID.", class = "ckdata_acceptance_deployment")
  }
  expected <- list(
    purpose = "ckdata-synthetic-tests", format_version = 1L,
    region = "us-west-2", database = "ck_datalake_tst_ckdata_fixtures", table = "types",
    bucket = paste0("ck-datalake-tst-ckdata-fixtures-", account)
  )
  purrr::iwalk(expected, function(value, field) {
    if (!identical(deployment[[field]], value)) {
      cli::cli_abort("{.code deployment${field}} must equal {.val {value}} with the same type.", class = "ckdata_acceptance_deployment")
    }
  })
  if (!rlang::is_list(deployment$seats) || !rlang::is_list(deployment$seats[[seat_name]])) {
    cli::cli_abort("{.code deployment$seats} must contain the selected {.val {seat_name}} seat.", class = "ckdata_acceptance_deployment")
  }
  seat <- deployment$seats[[seat_name]]
  expected_seat <- list(
    role_arn = paste0("arn:aws:iam::", account, ":role/ck-datalake-tst-ckdata-", seat_name, "-reader"),
    workgroup = paste0("ck-datalake-tst-ckdata-", seat_name),
    output_location = paste0("s3://", deployment$bucket, "/results/", seat_name, "/")
  )
  purrr::iwalk(expected_seat, function(value, field) {
    if (!identical(seat[[field]], value)) {
      cli::cli_abort("{.code deployment$seats${seat_name}${field}} must equal {.val {value}}.", class = "ckdata_acceptance_deployment")
    }
  })
  invisible(seat)
}

validate_live_workgroup <- function(info, seat, account) {
  if (!rlang::is_list(info)) {
    cli::cli_abort("Workgroup configuration must be an object.", class = "ckdata_acceptance_workgroup")
  }
  expected <- list(
    output_location = seat$output_location, expected_bucket_owner = account,
    encryption_option = "SSE_S3", kms_key = NULL,
    enforce_work_group_configuration = TRUE, selected_engine_version = "Athena engine version 3"
  )
  purrr::iwalk(expected, function(value, field) {
    if (!identical(info[[field]], value)) {
      cli::cli_abort("Workgroup {.field {field}} does not match the synthetic fixture contract.", class = "ckdata_acceptance_workgroup")
    }
  })
  if (!rlang::is_true(info$bytes_scanned_cutoff_per_query == 10 * 1024 * 1024)) {
    cli::cli_abort("Workgroup {.field bytes_scanned_cutoff_per_query} must equal 10 MiB.", class = "ckdata_acceptance_workgroup")
  }
  invisible(NULL)
}

# Helpers are sourced by ordinary tests; credential discovery starts only after
# this explicit gate and the synthetic deployment validation both succeed.
live_settings <- function() {
  testthat::skip_if_not(
    identical(Sys.getenv("CKDATA_RUN_LIVE_TESTS"), "true"),
    "Synthetic AWS tests require CKDATA_RUN_LIVE_TESTS=true"
  )
  path <- Sys.getenv("CKDATA_TEST_DEPLOYMENT")
  if (!nzchar(path) || !file.exists(path)) {
    rlang::abort("CKDATA_TEST_DEPLOYMENT must name the synthetic deployment JSON.", class = "ckdata_acceptance_usage")
  }
  seat_name <- Sys.getenv("CKDATA_TEST_SEAT")
  deployment <- jsonlite::read_json(path, simplifyVector = FALSE)
  seat <- validate_live_deployment(deployment, seat_name)
  list(deployment = deployment, seat = seat, seat_name = seat_name)
}

validate_live_connection <- function(connection, context) {
  deployment <- context$deployment
  info <- DBI::dbGetInfo(connection)
  expected <- list(account = deployment$account_id, schema = deployment$database, workgroup = context$seat$workgroup)
  purrr::iwalk(expected, function(value, field) {
    if (!identical(info[[field]], value)) {
      cli::cli_abort("Connection {.field {field}} does not match the synthetic fixture contract.", class = "ckdata_acceptance_identity")
    }
  })
  role <- paste0("ck-datalake-tst-ckdata-", context$seat_name, "-reader")
  prefix <- paste0("arn:aws:sts::", deployment$account_id, ":assumed-role/", role, "/")
  if (!rlang::is_string(info$arn) || !startsWith(info$arn, prefix)) {
    cli::cli_abort("Connection ARN must identify the selected synthetic fixture reader.", class = "ckdata_acceptance_identity")
  }
  configuration <- native_wait(native_workgroup(connection@state$handle))
  validate_live_workgroup(configuration, context$seat, deployment$account_id)
  invisible(NULL)
}

local_live_context <- function(env = rlang::caller_env()) {
  context <- live_settings()
  deployment <- context$deployment
  seat <- context$seat
  withr::local_options(list(
    ckdata.workgroup = seat$workgroup, ckdata.schema = deployment$database,
    ckdata.mode = "explore", ckdata.timeout = 120
  ), .local_envir = env)
  withr::local_envvar(c(
    CK_DATALAKE_INTERNAL_PROFILE = NA_character_, CK_DATALAKE_REGION = NA_character_,
    CK_DATALAKE_WORKGROUP = seat$workgroup, CK_DATALAKE_SCHEMA = deployment$database,
    CK_DATALAKE_ROLE_ARN = seat$role_arn, AWS_REGION = deployment$region
  ), .local_envir = env)
  rlang::local_bindings(con = NULL, tables = lake_state$tables[0, ], .env = lake_state, .frame = env)
  con <- DBI::dbConnect(ckdata::ckdata(),
    workgroup = seat$workgroup, schema = deployment$database,
    region = deployment$region, role_arn = seat$role_arn
  )
  close <- function(connection) {
    if (!rlang::is_null(connection)) {
      suppressWarnings(rlang::try_fetch(DBI::dbDisconnect(connection), error = function(cnd) NULL))
    }
  }
  withr::defer(
    {
      if (!identical(lake_state$con, con)) close(lake_state$con)
      close(con)
    },
    envir = env
  )
  validate_live_connection(con, context)
  context$con <- con
  context
}
