test_that("the default database is generic and explicit source settings take precedence", {
  withr::local_options(ckdata.schema = NULL)
  withr::local_envvar(CKDATA_SCHEMA = NA, CK_DATALAKE_SCHEMA = NA)
  expect_identical(ckdata_schema(), "default")
  withr::local_envvar(CK_DATALAKE_SCHEMA = "public_demo")
  expect_identical(ckdata_schema(), "public_demo")
  withr::local_envvar(CKDATA_SCHEMA = "canonical")
  expect_identical(ckdata_schema(), "canonical")
  withr::local_options(ckdata.schema = "my_project")
  expect_identical(ckdata_schema(), "my_project")
  withr::local_options(ckdata.schema = NULL)
  withr::local_envvar(CKDATA_SCHEMA = "")
  expect_identical(ckdata_schema(), "default")
})

test_that("legacy and canonical connection settings reach the native boundary without changing AWS settings", {
  fixture <- local_native_connection()
  withr::local_options(
    ckdata.workgroup = NULL,
    ckdata.schema = NULL,
    ckdata.login = FALSE
  )
  withr::local_envvar(c(
    CKDATA_WORKGROUP = NA,
    CKDATA_SCHEMA = NA,
    CKDATA_PROFILE = NA,
    CKDATA_REGION = NA,
    CKDATA_ROLE_ARN = NA,
    CKDATA_OUTPUT_LOCATION = NA,
    CK_DATALAKE_WORKGROUP = "legacy-workgroup",
    CK_DATALAKE_SCHEMA = "legacy_schema",
    CK_DATALAKE_INTERNAL_PROFILE = "legacy-profile",
    CK_DATALAKE_REGION = "eu-west-1",
    CK_DATALAKE_ROLE_ARN = "arn:aws:iam::000000000000:role/legacy",
    CK_DATALAKE_S3_STAGING_DIR = "s3://synthetic-legacy-results/",
    AWS_PROFILE = "standard-profile",
    AWS_REGION = "eu-central-1"
  ))
  aws <- Sys.getenv(c("AWS_PROFILE", "AWS_REGION"))
  legacy <- DBI::dbConnect(ckdata())
  withr::defer(DBI::dbDisconnect(legacy))
  expect_identical(
    tail(fixture$calls$connections, 1)[[1]],
    list(
      workgroup = "legacy-workgroup",
      schema = "legacy_schema",
      profile = "legacy-profile",
      region = "eu-west-1",
      role_arn = "arn:aws:iam::000000000000:role/legacy",
      output_location = "s3://synthetic-legacy-results/",
      sso = list(),
      login = FALSE
    )
  )
  withr::local_envvar(c(
    CKDATA_WORKGROUP = "new-workgroup",
    CKDATA_SCHEMA = "new_schema",
    CKDATA_PROFILE = "new-profile",
    CKDATA_REGION = "ap-southeast-2",
    CKDATA_ROLE_ARN = "arn:aws:iam::000000000000:role/new",
    CKDATA_OUTPUT_LOCATION = "s3://synthetic-new-results/"
  ))
  canonical <- DBI::dbConnect(ckdata())
  withr::defer(DBI::dbDisconnect(canonical))
  expect_identical(
    tail(fixture$calls$connections, 1)[[1]],
    list(
      workgroup = "new-workgroup",
      schema = "new_schema",
      profile = "new-profile",
      region = "ap-southeast-2",
      role_arn = "arn:aws:iam::000000000000:role/new",
      output_location = "s3://synthetic-new-results/",
      sso = list(),
      login = FALSE
    )
  )
  explicit <- DBI::dbConnect(
    ckdata(),
    workgroup = "explicit",
    schema = "explicit",
    profile = NULL,
    region = NULL,
    role_arn = NULL,
    output_location = NULL
  )
  withr::defer(DBI::dbDisconnect(explicit))
  expect_identical(
    tail(fixture$calls$connections, 1)[[1]],
    list(
      workgroup = "explicit",
      schema = "explicit",
      profile = NULL,
      region = NULL,
      role_arn = NULL,
      output_location = NULL,
      sso = list(),
      login = FALSE
    )
  )
  expect_identical(Sys.getenv(c("AWS_PROFILE", "AWS_REGION")), aws)
})

test_that("empty canonical settings mask legacy aliases and missing workgroups fail before connecting", {
  fixture <- local_native_connection()
  withr::local_options(
    ckdata.workgroup = NULL,
    ckdata.schema = NULL,
    ckdata.login = FALSE
  )
  withr::local_envvar(c(
    CKDATA_PROFILE = "",
    CKDATA_REGION = "",
    CKDATA_ROLE_ARN = "",
    CKDATA_OUTPUT_LOCATION = "",
    CKDATA_SCHEMA = "",
    CKDATA_WORKGROUP = "",
    CK_DATALAKE_INTERNAL_PROFILE = "legacy",
    CK_DATALAKE_REGION = "eu-west-1",
    CK_DATALAKE_ROLE_ARN = "arn:aws:iam::000000000000:role/legacy",
    CK_DATALAKE_S3_STAGING_DIR = "s3://synthetic-legacy-results/",
    CK_DATALAKE_SCHEMA = "legacy",
    CK_DATALAKE_WORKGROUP = "legacy"
  ))
  expect_error(DBI::dbConnect(ckdata()), "set CKDATA_WORKGROUP")
  expect_length(fixture$calls$connections, 1L)
  con <- DBI::dbConnect(ckdata(), workgroup = "explicit")
  withr::defer(DBI::dbDisconnect(con))
  expect_identical(
    tail(fixture$calls$connections, 1)[[1]],
    list(
      workgroup = "explicit",
      schema = "default",
      profile = NULL,
      region = NULL,
      role_arn = NULL,
      output_location = NULL,
      sso = list(),
      login = FALSE
    )
  )
})

test_that("managed table and SQL helpers share the configured session", {
  fixture <- local_native_connection()
  rlang::local_bindings(
    con = NULL,
    tables = lake_state$tables[0, ],
    .env = lake_state
  )
  withr::local_envvar(
    CKDATA_WORKGROUP = NA,
    CKDATA_PROFILE = NA,
    CKDATA_REGION = NA,
    CK_DATALAKE_WORKGROUP = "synthetic",
    CK_DATALAKE_INTERNAL_PROFILE = "synthetic-profile",
    CK_DATALAKE_REGION = "eu-west-1",
    CK_DATALAKE_S3_STAGING_DIR = NA,
    CK_DATALAKE_ROLE_ARN = NA
  )
  withr::local_options(ckdata.workgroup = NULL, ckdata.schema = "fixture")
  lake_tbl("types")
  withr::defer(lake_disconnect())
  expect_length(fixture$calls$queries, 0L)
  lake_query("SELECT id FROM types")
  lake_tbl("types")
  expect_identical(fixture$calls$queries, "SELECT id FROM types")
  # The fixture opens one explicit connection; all managed calls share one more.
  expect_length(fixture$calls$connections, 2L)
  args <- tail(fixture$calls$connections, 1)[[1]]
  expect_identical(args$profile, "synthetic-profile")
  expect_identical(args$region, "eu-west-1")
  expect_false("s3_staging_dir" %in% names(args))
})

test_that("a serialized result is invalid even if its R state still says open", {
  con <- local_native_connection()$con
  result <- DBI::dbSendQuery(con, "SELECT synthetic")
  restored <- unserialize(serialize(result, NULL))
  expect_false(DBI::dbIsValid(restored))
  DBI::dbClearResult(result)
})

test_that("configuration changes replace a managed connection only after explicit disconnect", {
  fixture <- local_native_connection()
  rlang::local_bindings(con = fixture$con, .env = lake_state)
  withr::local_options(ckdata.workgroup = "synthetic")
  withr::local_envvar(CK_DATALAKE_ROLE_ARN = NA)
  expect_true(DBI::dbIsValid(fixture$con))
  expect_identical(lake_con(), fixture$con)
  expect_silent(lake_query("SELECT 1"))
  expect_identical(fixture$calls$queries, "SELECT 1")
  expect_length(fixture$calls$connections, 1L)
  expect_true(lake_disconnect())
  expect_false(fixture$con@state$open)
  expect_true(lake_disconnect())
  replacement <- lake_con()
  withr::defer(lake_disconnect())
  expect_false(rlang::is_reference(replacement@state, fixture$con@state))
  expect_length(fixture$calls$connections, 2L)
  expect_true(DBI::dbIsValid(replacement))
})

test_that("disconnecting an unused managed session never opens a connection", {
  fixture <- local_native_connection()
  rlang::local_bindings(con = NULL, .env = lake_state)
  expect_true(lake_disconnect())
  expect_length(fixture$calls$connections, 1L)
})
