test_that("the handoff keeps NULL, the empty string and 64-bit integers", {
  x <- arrow_probe()
  expect_true(is.na(x$s_null[1]))
  expect_identical(x$s_empty[1], "")
  expect_identical(x$big[1], bit64::as.integer64("9007199254740993"))
  # Compared in whole milliseconds: expect_equal's tolerance at this magnitude is about 27 seconds.
  expect_identical(round(as.numeric(x$ts[1]) * 1000), 1792154096789)
  expect_identical(attr(x$ts, "tzone"), "UTC")
})

test_that("R and native stream entrypoints reject extra arguments", {
  expect_error(arrow_probe_stream("4096"), "unused argument")
  expect_error(.Call(wrap__arrow_probe_stream, "4096"), "Incorrect number of arguments")
})

test_that("the producer owns its stream across garbage collection and export", {
  stream <- arrow_probe_stream()
  expect_s3_class(stream, "nanoarrow_array_stream")
  gc()
  destination <- nanoarrow::nanoarrow_allocate_array_stream()
  nanoarrow::nanoarrow_pointer_export(stream, destination)
  rm(stream)
  gc()
  expect_true(nanoarrow::nanoarrow_pointer_is_valid(destination))
  expect_equal(nanoarrow::convert_array_stream(destination)$s_empty[1], "")
})
