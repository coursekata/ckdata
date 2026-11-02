test_that("the handoff keeps NULL, the empty string and 64-bit integers", {
  x <- arrow_probe()
  expect_true(is.na(x$s_null[1]))
  expect_identical(x$s_empty[1], "ci-negative-control")
  expect_identical(x$big[1], bit64::as.integer64("9007199254740993"))
  # Compared in whole milliseconds: expect_equal's tolerance at this magnitude is about 27 seconds.
  expect_identical(round(as.numeric(x$ts[1]) * 1000), 1792154096789)
  expect_identical(attr(x$ts, "tzone"), "UTC")
})
