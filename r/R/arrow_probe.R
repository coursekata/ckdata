#' The core's literal probe as a tibble, through a nanoarrow stream.
#'
#' 64-bit integer columns come back as `bit64::integer64` so values past 2^53 stay exact.
#' @noRd
arrow_probe <- function() {
  stream <- arrow_probe_stream()
  schema <- nanoarrow::infer_nanoarrow_schema(stream)
  ptype <- nanoarrow::infer_nanoarrow_ptype(schema)
  is_int64 <- purrr::map_lgl(schema$children, function(field) field$format == "l")
  ptype[is_int64] <- purrr::map(ptype[is_int64], function(x) bit64::integer64())
  tibble::as_tibble(nanoarrow::convert_array_stream(stream, to = ptype))
}
