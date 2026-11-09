test_that("the shipped column table holds, as text, every field the check reads", {
  expect_named(
    column_changes,
    c("table", "column", "new_table", "new_column", "removed_on")
  )
  expect_true(all(vapply(column_changes, rlang::is_character, logical(1))))
})

test_that("every shipped row names its table and column and has a valid removal date", {
  expect_false(anyNA(column_changes[c("table", "column", "removed_on")]))
  expect_false(anyNA(as.Date(column_changes$removed_on, format = "%Y-%m-%d")))
})

test_that("each kind of replacement gets its own note", {
  notes <- deprecation_note(
    table = rep("mart_responses", 4),
    new_table = c("mart_responses", "mart_enrollments", "mart_questions", NA),
    new_column = c("question_type", "variation_index", NA, NA),
    removed_on = "2026-12-01"
  )

  expect_equal(
    notes,
    c(
      "deprecated, removed on 2026-12-01; use `question_type`",
      "deprecated, removed on 2026-12-01; use `variation_index` on mart_enrollments",
      "deprecated, removed on 2026-12-01; see mart_questions",
      "deprecated, removed on 2026-12-01, with no replacement"
    )
  )
})
