# Relative equality on epoch-sized numbers can hide a lost fractional second.
expect_timestamp_equal <- function(actual, expected, tolerance = 1e-4) {
  testthat::expect_s3_class(actual, "POSIXct")
  testthat::expect_identical(is.na(actual), is.na(expected))
  difference <- abs(as.numeric(actual) - as.numeric(expected))
  testthat::expect_lte(max(c(0, difference), na.rm = TRUE), tolerance)
}

validate_deployment <- function(deployment, seat_name) {
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

validate_workgroup <- function(info, seat, account) {
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
