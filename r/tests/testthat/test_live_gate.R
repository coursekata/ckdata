synthetic_deployment <- function() {
  account <- "000000000000"
  bucket <- paste0("ck-datalake-tst-ckdata-fixtures-", account)
  list(
    purpose = "ckdata-synthetic-tests",
    format_version = 1L,
    account_id = account,
    region = "us-west-2",
    database = "ck_datalake_tst_ckdata_fixtures",
    table = "types",
    bucket = bucket,
    seats = list(
      internal = list(
        role_arn = paste0(
          "arn:aws:iam::",
          account,
          ":role/ck-datalake-tst-ckdata-internal-reader"
        ),
        workgroup = "ck-datalake-tst-ckdata-internal",
        output_location = paste0("s3://", bucket, "/results/internal/")
      )
    )
  )
}

local_live_contract <- function(
  deployment = synthetic_deployment(),
  env = rlang::caller_env()
) {
  path <- withr::local_tempfile(fileext = ".json", .local_envir = env)
  jsonlite::write_json(deployment, path, auto_unbox = TRUE)
  withr::local_envvar(
    c(
      CKDATA_RUN_LIVE_TESTS = "true",
      CKDATA_TEST_DEPLOYMENT = path,
      CKDATA_TEST_SEAT = "internal"
    ),
    .local_envir = env
  )
}

test_that("live tests skip before authentication unless explicitly enabled", {
  withr::local_envvar(c(CKDATA_RUN_LIVE_TESTS = NA_character_))
  calls <- 0L
  testthat::local_mocked_bindings(
    dbConnect = function(...) {
      calls <<- calls + 1L
      rlang::abort("unexpected authentication")
    },
    .package = "DBI"
  )
  condition <- rlang::catch_cnd(local_live_context(), classes = "skip")
  expect_s3_class(condition, "skip")
  expect_identical(calls, 0L)
})

test_that("enabled live tests fail for missing or invalid deployment settings before authentication", {
  withr::local_envvar(c(
    CKDATA_RUN_LIVE_TESTS = "true",
    CKDATA_TEST_DEPLOYMENT = NA_character_
  ))
  calls <- 0L
  testthat::local_mocked_bindings(
    dbConnect = function(...) {
      calls <<- calls + 1L
      rlang::abort("unexpected authentication")
    },
    .package = "DBI"
  )
  expect_error(local_live_context(), class = "ckdata_acceptance_usage")
  changed <- synthetic_deployment()
  changed$region <- "us-east-1"
  local_live_contract(changed)
  expect_error(
    local_live_context(),
    "deployment$region",
    class = "ckdata_acceptance_deployment",
    fixed = TRUE
  )
  expect_identical(calls, 0L)
})

test_that("live fixtures restore options, environment and managed state after connection failure", {
  local_live_contract()
  withr::local_options(ckdata.mode = "sentinel", ckdata.timeout = -1)
  variables <- c(
    CK_DATALAKE_INTERNAL_PROFILE = "sentinel",
    CK_DATALAKE_REGION = "sentinel",
    CK_DATALAKE_WORKGROUP = "sentinel",
    CK_DATALAKE_SCHEMA = "sentinel",
    CK_DATALAKE_ROLE_ARN = "sentinel",
    AWS_REGION = "sentinel"
  )
  withr::local_envvar(variables)
  original <- rlang::env_get(lake_state, "con", default = NULL)
  tables <- lake_state$tables
  testthat::local_mocked_bindings(
    dbConnect = function(...) {
      expect_identical(getOption("ckdata.mode"), "explore")
      expect_identical(
        Sys.getenv("CK_DATALAKE_WORKGROUP"),
        "ck-datalake-tst-ckdata-internal"
      )
      expect_identical(
        Sys.getenv("CK_DATALAKE_INTERNAL_PROFILE", unset = NA_character_),
        NA_character_
      )
      rlang::abort(
        "Synthetic pre-connection failure",
        class = "synthetic_connection_stop"
      )
    },
    .package = "DBI"
  )
  attempt <- function() local_live_context()
  expect_error(attempt(), class = "synthetic_connection_stop")
  expect_identical(getOption("ckdata.mode"), "sentinel")
  expect_identical(getOption("ckdata.timeout"), -1)
  expect_identical(Sys.getenv(names(variables)), variables)
  expect_identical(rlang::env_get(lake_state, "con", default = NULL), original)
  expect_identical(lake_state$tables, tables)
})

test_that("an identity mismatch closes the live fixture without submitting a query", {
  local_live_contract()
  fixture <- local_native_connection()
  testthat::local_mocked_bindings(
    dbConnect = function(...) fixture$con,
    .package = "DBI"
  )
  attempt <- function() local_live_context()
  expect_error(attempt(), class = "ckdata_acceptance_identity")
  expect_false(fixture$con@state$open)
  expect_length(fixture$calls$queries, 0L)
})

test_that("R acceptance rejects deployment drift before authentication", {
  deployment <- synthetic_deployment()
  expect_identical(
    validate_live_deployment(deployment, "internal"),
    deployment$seats$internal
  )
  for (version in list(1, "1", NA_integer_, c(1L, 1L))) {
    changed <- deployment
    changed$format_version <- version
    expect_error(
      validate_live_deployment(changed, "internal"),
      "deployment$format_version",
      class = "ckdata_acceptance_deployment",
      fixed = TRUE
    )
  }
  for (field in c(
    "purpose",
    "format_version",
    "account_id",
    "region",
    "database",
    "table",
    "bucket"
  )) {
    changed <- deployment
    changed[[field]] <- "invalid"
    expect_error(
      validate_live_deployment(changed, "internal"),
      paste0("deployment$", field),
      class = "ckdata_acceptance_deployment",
      fixed = TRUE
    )
  }
  for (field in c("role_arn", "workgroup", "output_location")) {
    changed <- deployment
    changed$seats$internal[[field]] <- "invalid"
    expect_error(
      validate_live_deployment(changed, "internal"),
      paste0("deployment$seats$internal$", field),
      class = "ckdata_acceptance_deployment",
      fixed = TRUE
    )
  }
})

test_that("timestamp acceptance detects truncation and isolated precision errors", {
  expected <- as.POSIXct(
    c(1792154096.789, 1792154096.789),
    origin = "1970-01-01",
    tz = "UTC"
  )
  expect_no_error(expect_timestamp_equal(expected, expected))
  truncated <- as.POSIXct(
    trunc(as.numeric(expected)),
    origin = "1970-01-01",
    tz = "UTC"
  )
  expect_s3_class(
    rlang::catch_cnd(
      expect_timestamp_equal(truncated, expected),
      classes = "expectation_failure"
    ),
    "expectation_failure"
  )
  corrupted <- expected
  corrupted[1] <- corrupted[1] + 0.00015
  expect_s3_class(
    rlang::catch_cnd(
      expect_timestamp_equal(corrupted, expected),
      classes = "expectation_failure"
    ),
    "expectation_failure"
  )
})

test_that("R acceptance rejects live workgroup drift before querying", {
  deployment <- synthetic_deployment()
  seat <- deployment$seats$internal
  info <- native_probe_workgroup()
  expect_null(info$kms_key)
  expect_no_error(validate_live_workgroup(info, seat, deployment$account_id))
  changes <- list(
    output_location = "s3://unrelated/",
    expected_bucket_owner = "111111111111",
    encryption_option = "SSE_KMS",
    kms_key = "unexpected-key",
    enforce_work_group_configuration = FALSE,
    bytes_scanned_cutoff_per_query = 20 * 1024 * 1024,
    selected_engine_version = "Athena engine version 2"
  )
  for (field in names(changes)) {
    changed <- info
    changed[[field]] <- changes[[field]]
    expect_error(
      validate_live_workgroup(changed, seat, deployment$account_id),
      field,
      class = "ckdata_acceptance_workgroup",
      fixed = TRUE
    )
    changed[field] <- list(NULL)
    if (field != "kms_key") {
      expect_error(
        validate_live_workgroup(changed, seat, deployment$account_id),
        field,
        class = "ckdata_acceptance_workgroup",
        fixed = TRUE
      )
    }
  }
})
