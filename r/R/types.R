#' Result types and ordering
#'
#' Data-frame and tibble output use Arrow's R conversion: booleans become
#' logical vectors, 8/16/32-bit integers become R integers, floating point and
#' decimals become doubles, strings become character vectors, dates become
#' Date, and timestamps become POSIXct. Arrow's integer64 downcasting is disabled:
#' 64-bit integers consistently use bit64::integer64, including inside nested
#' values and when results are empty. Lists/maps/structs use Arrow's established
#' nested R representations.
#'
#' R doubles cannot represent every decimal exactly or retain every timestamp's
#' subsecond precision. R reserves the minimum signed 32-bit integer for NA;
#' bit64 reserves the minimum signed 64-bit value for NA.
#' Request `lake_query(..., as = "arrow")` for the original Arrow types and
#' their full precision/range. No additional coercion is applied to that path.
#' R Date/POSIXt values used in SQL are encoded as typed DATE/TIMESTAMP literals.
#' Timestamp literals use UTC instants; strings that resemble dates remain strings.
#' difftime inputs use DBI's TIME formatter, including their units and fractions.
#' These values can be used in predicates; Athena UNLOAD rejects TIME output columns.
#' Athena UNLOAD timestamps have no timezone annotation. Arrow's R conversion
#' therefore returns POSIXct without a named timezone; local display depends on
#' the R session's timezone. The Arrow output retains the original naive type.
#' INT96 timestamps use the upstream reader's microsecond schema hint to retain
#' Athena's full date range; native nanosecond timestamp columns keep their precision.
#'
#' NULL and empty strings remain distinct. Empty results retain their columns
#' and corresponding types. Cached tibble results use the same conversions.
#'
#' Raw SQL follows Athena UNLOAD's output restrictions: expressions need
#' explicit names and standalone NULL outputs need explicit types. For example,
#' use `SELECT 1 AS example` and `SELECT CAST(NULL AS VARCHAR) AS example`.
#' Duplicate output names and types unsupported by UNLOAD are also rejected by
#' Athena. This interface supports a read-side DBI subset, not full conformance.
#'
#' Parquet UNLOAD does not guarantee order across files. SQL ORDER BY and
#' remote arrange() do not create a global downloaded-order contract; arrange
#' the collected result locally when needed.
#'
#' `grepl()` translates to Athena's Unicode Java regular expressions. Literal
#' matching supports `fixed = TRUE`; `perl` and `useBytes` are unsupported.
#' NULL text matches FALSE, while a missing pattern returns a missing logical.
#' `floor_date()` supports whole seconds through years, including quarters.
#' Weeks honor `week_start` (Monday = 1, Sunday = 7), defaulting to the
#' `lubridate.week.start` option or Sunday.
#' @name ckdata_types
NULL
