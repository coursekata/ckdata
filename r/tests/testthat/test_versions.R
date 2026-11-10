test_that("the package version is the core's", {
  expect_identical(
    core_versions()[1],
    paste("ckdata", utils::packageVersion("ckdata"))
  )
})
