test_that("ckdata.mode defaults to explore", {
  withr::local_options(ckdata.mode = NULL)

  expect_equal(ckdata_mode(), "explore")
})

test_that("an unknown ckdata.mode fails, naming the modes", {
  withr::local_options(ckdata.mode = "strict")

  expect_error(ckdata_mode(), "explore.*audit.*publish")
})

test_that("ckdata.allow must be a named list of column names", {
  for (allow in list(c(mart_responses = "lrn_type"), list("lrn_type"), list(mart_responses = 1))) {
    withr::with_options(list(ckdata.allow = allow), {
      expect_error(ckdata_allow(), "named list", class = "ckdata_bad_option")
    })
  }
})
