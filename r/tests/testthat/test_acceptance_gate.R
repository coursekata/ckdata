test_that("live acceptance requires explicit opt-in before any AWS client", {
  runner <- testthat::test_path("..", "acceptance", "query_acceptance.R")
  output <- suppressWarnings(system2(file.path(R.home("bin"), "Rscript"),
    c("--vanilla", shQuote(runner)),
    stdout = TRUE, stderr = TRUE
  ))
  expect_identical(attr(output, "status"), 1L)
  expect_match(paste(output, collapse = "\n"), "No AWS clients created", fixed = TRUE)
})

acceptance_helpers <- rlang::new_environment(parent = rlang::base_env())
sys.source(testthat::test_path("..", "acceptance", "helper_acceptance.R"), acceptance_helpers)

synthetic_deployment <- function() {
  account <- "000000000000"
  bucket <- paste0("ck-datalake-tst-ckdata-fixtures-", account)
  list(
    purpose = "ckdata-synthetic-tests", format_version = 1L, account_id = account,
    region = "us-west-2", database = "ck_datalake_tst_ckdata_fixtures", table = "types",
    bucket = bucket,
    seats = list(internal = list(
      role_arn = paste0("arn:aws:iam::", account, ":role/ck-datalake-tst-ckdata-internal-reader"),
      workgroup = "ck-datalake-tst-ckdata-internal",
      output_location = paste0("s3://", bucket, "/results/internal/")
    ))
  )
}

test_that("R acceptance rejects deployment drift before authentication", {
  deployment <- synthetic_deployment()
  expect_identical(acceptance_helpers$validate_deployment(deployment, "internal"), deployment$seats$internal)
  for (version in list(1, "1", NA_integer_, c(1L, 1L))) {
    changed <- deployment
    changed$format_version <- version
    expect_error(acceptance_helpers$validate_deployment(changed, "internal"), "deployment$format_version", class = "ckdata_acceptance_deployment", fixed = TRUE)
  }
  for (field in c("purpose", "format_version", "account_id", "region", "database", "table", "bucket")) {
    changed <- deployment
    changed[[field]] <- "invalid"
    expect_error(acceptance_helpers$validate_deployment(changed, "internal"), paste0("deployment$", field), class = "ckdata_acceptance_deployment", fixed = TRUE)
  }
  for (field in c("role_arn", "workgroup", "output_location")) {
    changed <- deployment
    changed$seats$internal[[field]] <- "invalid"
    expect_error(acceptance_helpers$validate_deployment(changed, "internal"), paste0("deployment$seats$internal$", field), class = "ckdata_acceptance_deployment", fixed = TRUE)
  }
})

test_that("timestamp acceptance detects truncation and isolated precision errors", {
  expected <- as.POSIXct(c(1792154096.789, 1792154096.789), origin = "1970-01-01", tz = "UTC")
  expect_no_error(acceptance_helpers$expect_timestamp_equal(expected, expected))
  truncated <- as.POSIXct(trunc(as.numeric(expected)), origin = "1970-01-01", tz = "UTC")
  expect_s3_class(rlang::catch_cnd(acceptance_helpers$expect_timestamp_equal(truncated, expected), classes = "expectation_failure"), "expectation_failure")
  corrupted <- expected
  corrupted[1] <- corrupted[1] + 0.00015
  expect_s3_class(rlang::catch_cnd(acceptance_helpers$expect_timestamp_equal(corrupted, expected), classes = "expectation_failure"), "expectation_failure")
})

test_that("acceptance restores session settings after a pre-connection failure", {
  runner <- parse(testthat::test_path("..", "acceptance", "query_acceptance.R"))
  definition <- purrr::detect(as.list(runner), function(expr) {
    rlang::is_call(expr, "<-") && rlang::is_symbol(expr[[2]], "main")
  })
  scope <- rlang::env(
    seat = list(workgroup = "synthetic-workgroup", role_arn = "synthetic-role"),
    deployment = list(database = "synthetic-database", region = "synthetic-region")
  )
  rlang::eval_bare(definition, scope)
  withr::local_options(ckdata.mode = "sentinel", ckdata.timeout = -1)
  variables <- c(
    CK_DATALAKE_INTERNAL_PROFILE = "sentinel", CK_DATALAKE_REGION = "sentinel",
    CK_DATALAKE_WORKGROUP = "sentinel", CK_DATALAKE_SCHEMA = "sentinel",
    CK_DATALAKE_ROLE_ARN = "sentinel", AWS_REGION = "sentinel"
  )
  withr::local_envvar(variables)
  testthat::local_mocked_bindings(dbConnect = function(...) {
    expect_identical(getOption("ckdata.mode"), "explore")
    expect_identical(Sys.getenv("CK_DATALAKE_WORKGROUP"), "synthetic-workgroup")
    expect_identical(Sys.getenv("CK_DATALAKE_INTERNAL_PROFILE", unset = NA_character_), NA_character_)
    rlang::abort("Synthetic pre-connection failure", class = "synthetic_connection_stop")
  }, .package = "DBI")
  expect_error(scope$main(), class = "synthetic_connection_stop")
  expect_identical(getOption("ckdata.mode"), "sentinel")
  expect_identical(getOption("ckdata.timeout"), -1)
  expect_identical(Sys.getenv(names(variables)), variables)
})

test_that("the actual R runner rejects a different region before authentication", {
  runner <- normalizePath(testthat::test_path("..", "acceptance", "query_acceptance.R"))
  contract <- tempfile(fileext = ".json")
  withr::defer(unlink(contract))
  deployment <- synthetic_deployment()
  deployment$region <- "us-east-1"
  jsonlite::write_json(deployment, contract, auto_unbox = TRUE)
  calls <- 0L
  testthat::local_mocked_bindings(native_connect = function(...) {
    calls <<- calls + 1L
    rlang::abort("unexpected authentication")
  }, .package = "ckdata")
  env <- rlang::env(rlang::global_env())
  env$commandArgs <- function(trailingOnly = FALSE) {
    if (trailingOnly) c("--run-live", contract, "internal") else paste0("--file=", runner)
  }
  expect_error(sys.source(runner, env), "deployment\\$region")
  expect_identical(calls, 0L)
})

test_that("R acceptance rejects live workgroup drift before querying", {
  deployment <- synthetic_deployment()
  seat <- deployment$seats$internal
  info <- native_probe_workgroup()
  expect_null(info$kms_key)
  expect_no_error(acceptance_helpers$validate_workgroup(info, seat, deployment$account_id))
  changes <- list(
    output_location = "s3://unrelated/", expected_bucket_owner = "111111111111",
    encryption_option = "SSE_KMS", kms_key = "unexpected-key",
    enforce_work_group_configuration = FALSE, bytes_scanned_cutoff_per_query = 20 * 1024 * 1024,
    selected_engine_version = "Athena engine version 2"
  )
  for (field in names(changes)) {
    changed <- info
    changed[[field]] <- changes[[field]]
    expect_error(acceptance_helpers$validate_workgroup(changed, seat, deployment$account_id), field, class = "ckdata_acceptance_workgroup", fixed = TRUE)
    changed[field] <- list(NULL)
    if (field != "kms_key") expect_error(acceptance_helpers$validate_workgroup(changed, seat, deployment$account_id), field, class = "ckdata_acceptance_workgroup", fixed = TRUE)
  }
})
