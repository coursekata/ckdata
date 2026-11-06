test_that("the standard DBI driver and read-side connection contracts hold", {
  skip_if_not_installed("DBItest")
  local_native_connection()
  ctx <- DBItest::make_context(ckdata(),
    connect_args = list(workgroup = "synthetic", schema = "fixture", role_arn = NULL),
    set_as_default = FALSE,
    tweaks = DBItest::tweaks(constructor_name = "ckdata", dbitest_version = "1.8.3")
  )
  # The driver intentionally has one fixed bigint conversion, integer64.
  # Write/transaction tests are not applicable to this read-only interface.
  DBItest::test_driver(ctx = ctx, run_only = c(
    "constructor", "data_type_formals",
    "data_type_driver", "get_info_driver", "connect_formals", "connect_can_connect", "connect_format"
  ))
  DBItest::test_connection(ctx = ctx, run_only = c(
    "disconnect_formals", "can_disconnect",
    "disconnect_closed_connection", "data_type_connection", "get_info_connection"
  ))
  DBItest::test_sql(ctx = ctx, run_only = c(
    "quote_string_formals", "quote_string_return",
    "quote_string_vectorized", "quote_string_double", "quote_string_error",
    "quote_identifier_formals", "quote_identifier_return", "quote_identifier_vectorized",
    "quote_identifier_error",
    "quote_literal_formals", "quote_literal_return", "quote_literal_vectorized",
    "quote_literal_empty", "quote_literal_double", "quote_literal_error",
    "unquote_identifier_formals", "unquote_identifier_return", "unquote_identifier_vectorized",
    "unquote_identifier_plain", "unquote_identifier_error"
  ))
  DBItest::test_result(ctx = ctx, run_only = c(
    "send_query_formals", "fetch_formals", "clear_result_formals", "get_query_formals"
  ))
  DBItest::test_meta(ctx = ctx, run_only = c(
    "is_valid_formals", "has_completed_formals", "get_statement_formals", "column_info_formals",
    "get_row_count_formals", "get_rows_affected_formals", "get_info_result"
  ))
  DBItest::test_arrow(ctx = ctx, run_only = c(
    "arrow_send_query_formals", "arrow_fetch_arrow_formals", "arrow_fetch_arrow_chunk_formals",
    "arrow_get_query_arrow_formals", "arrow_read_table_arrow_formals"
  ))
  # Live round trips use aliases/explicit NULL types in synthetic acceptance.
  # The unmodified DBItest templates violate UNLOAD's named/typed output
  # requirements; this selected suite does not claim full DBI conformance.
})

test_that("explicit DBI settings reach native connection setup and reject invalid values", {
  fixture <- local_native_connection()
  con <- DBI::dbConnect(ckdata(),
    workgroup = "another-workgroup", schema = "another-schema",
    profile = "synthetic-profile", region = "eu-west-1", role_arn = "arn:aws:iam::000000000000:role/synthetic"
  )
  withr::defer(DBI::dbDisconnect(con))
  args <- tail(fixture$calls$connections, 1)[[1]]
  expect_identical(args$profile, "synthetic-profile")
  expect_identical(args$region, "eu-west-1")
  expect_identical(args$workgroup, "another-workgroup")
  expect_identical(args$schema, "another-schema")
  expect_identical(args$role_arn, "arn:aws:iam::000000000000:role/synthetic")
  expect_error(DBI::dbConnect(ckdata(), workgroup = ""), "`workgroup` must be")
  expect_error(DBI::dbConnect(ckdata(), workgroup = "valid", profile = NA_character_), "`profile` must be")
})
