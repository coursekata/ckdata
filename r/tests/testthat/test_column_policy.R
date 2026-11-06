flagged <- "ckdata_flagged_columns"
responses_columns <- c("class_id", "question_type", "lrn_type", "branch")
responses <- simulated_tbl("mart_responses", responses_columns)
mart_classes <- simulated_tbl("mart_classes", c("class_id", "branch"))
int_classes <- simulated_tbl(
  "int_classes", c("class_id", "is_test"), "ck_datalake_prd_intermediate_app"
)
stg_books <- simulated_tbl(
  "stg_books", c("class_id", "book_name"), "ck_datalake_prd_staging_app"
)

# The column check on query `x`, given the lake tables the registry holds, with the fixture rows.
check <- function(x, ..., mode, allow = list()) {
  tables <- registry_of(...)
  apply_column_policy(x, tables, mode = mode, allow = allow, changes = deprecation_fixture)
}

test_that("explore mode leaves the query alone and says nothing", {
  expect_no_warning(out <- check(responses, responses, mode = "explore"))
  expect_identical(out, responses)
})

test_that("audit mode names each deprecated column a query returns, with its replacement", {
  expect_warning(
    out <- check(responses, responses, mode = "audit"),
    "lrn_type.*question_type",
    class = flagged
  )
  expect_warning(
    check(responses, responses, mode = "audit"),
    "branch.*variation_index.*mart_enrollments"
  )
  expect_equal(colnames(out), responses_columns)
})

test_that("a deprecated column used only to filter is reported but not removed", {
  x <- responses |>
    dplyr::filter(lrn_type == "mcq") |>
    dplyr::select(class_id)

  expect_warning(out <- check(x, responses, mode = "publish"), "lrn_type", class = flagged)
  expect_equal(colnames(out), "class_id")
})

test_that("publish mode removes the deprecated columns a query would return", {
  expect_warning(out <- check(responses, responses, mode = "publish"), class = flagged)
  expect_equal(colnames(out), c("class_id", "question_type"))
})

test_that("publish mode removes deprecated columns that come through a join", {
  names <- simulated_tbl("mart_classes", c("class_id", "class_name"))
  x <- dplyr::left_join(responses, names, by = "class_id")

  expect_warning(out <- check(x, responses, names, mode = "publish"), class = flagged)
  expect_equal(colnames(out), c("class_id", "question_type", "class_name"))
})

test_that("publish mode errors when every column a query returns is flagged", {
  x <- responses |> dplyr::select(lrn_type)

  expect_error(check(x, responses, mode = "publish"), "lrn_type", class = flagged)
})

test_that("an allowlisted column is neither reported nor removed", {
  allow <- list(mart_responses = c("lrn_type", "branch"))

  expect_no_warning(out <- check(responses, responses, mode = "publish", allow = allow))
  expect_equal(colnames(out), responses_columns)
})

test_that("a column the query defines under a deprecated name is the query's own", {
  base <- simulated_tbl("mart_responses", c("class_id", "question_type", "lrn_type"))
  x <- base |> dplyr::mutate(lrn_type = question_type)

  expect_no_warning(out <- check(x, base, mode = "publish"))
  expect_equal(colnames(out), c("class_id", "question_type", "lrn_type"))
})

test_that("a deprecation applies only to the table that declares it", {
  expect_no_warning(check(mart_classes, responses, mart_classes, mode = "audit"))
})

test_that("columns only a staging or intermediate table provides are flagged, not shared keys", {
  x <- responses |>
    dplyr::left_join(int_classes, by = "class_id") |>
    dplyr::left_join(stg_books, by = "class_id")

  cnd <- expect_warning(check(x, responses, int_classes, stg_books, mode = "audit"))
  expect_match(rlang::cnd_message(cnd), "is_test.*int_classes.*intermediate")
  expect_match(rlang::cnd_message(cnd), "book_name.*stg_books.*staging")
  expect_no_match(rlang::cnd_message(cnd), "`class_id`")
})

test_that("a query that reads only an intermediate table, with nothing allowed, cannot publish", {
  expect_error(check(int_classes, int_classes, mode = "publish"), class = flagged)

  allow <- list(int_classes = c("class_id", "is_test"))
  expect_no_warning(check(int_classes, int_classes, mode = "publish", allow = allow))
})

test_that("managed native tables apply policy once and do not change explicit tables", {
  fixture <- local_native_connection()
  local_lake_session(fixture$con)
  withr::local_options(ckdata.mode = "audit")
  managed <- lake_tbl("int_types", schema = "fixture")
  expect_length(testthat::capture_warnings(dplyr::collect(managed)), 1L)
  direct <- dplyr::tbl(fixture$con, DBI::Id(schema = "fixture", table = "int_types"))
  withr::local_options(ckdata.mode = "publish", ckdata.allow = list(int_types = "id"))
  expect_no_warning(dplyr::collect(direct))
  expect_match(tail(fixture$calls$queries, 1), "SELECT *", fixed = TRUE)
  expect_warning(dplyr::collect(managed), class = "ckdata_flagged_columns")
  expect_match(tail(fixture$calls$queries, 1), 'SELECT "id"', fixed = TRUE)
  expect_no_warning(dplyr::collect(direct))
  expect_match(tail(fixture$calls$queries, 1), "SELECT *", fixed = TRUE)
  collapsed <- dplyr::collapse(managed)
  expect_warning(dplyr::collect(collapsed), class = "ckdata_flagged_columns")
  expect_match(tail(fixture$calls$queries, 1), 'SELECT "id"', fixed = TRUE)
})
