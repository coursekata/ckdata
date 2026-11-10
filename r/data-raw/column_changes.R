# Refresh the shipped deprecation aliases from ck-datalake's versioned metadata.
# Run from r/: Rscript data-raw/column_changes.R [path/to/column_changes.csv]
# This reads schema-change metadata only; it never connects to the data lake.
args <- commandArgs(trailingOnly = TRUE)
source_csv <- if (length(args)) {
  args[[1]]
} else {
  "../../ck-datalake/dbt-runner/dbt/column_changes.csv"
}
if (!file.exists(source_csv)) {
  cli::cli_abort(
    "Column-change metadata not found: {.path {source_csv}}.",
    class = "ckdata_column_changes_missing"
  )
}

changes <- utils::read.csv(
  source_csv,
  colClasses = "character",
  na.strings = "",
  check.names = FALSE
)
fields <- c("table", "column", "new_table", "new_column", "removed_on")
missing <- setdiff(fields, names(changes))
if (length(missing)) {
  cli::cli_abort(
    "Column-change metadata is missing required fields: {.field {missing}}.",
    class = "ckdata_column_changes_schema"
  )
}
column_changes <- changes |>
  tibble::as_tibble() |>
  dplyr::filter(!is.na(column), !is.na(removed_on)) |>
  dplyr::select(dplyr::all_of(fields)) |>
  dplyr::arrange(table, column)
save(column_changes, file = "R/sysdata.rda", compress = "xz", version = 3)
