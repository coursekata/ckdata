# column_changes (R/sysdata.rda, built by data-raw/column_changes.R) holds the deprecated columns
# the lake still publishes: one row per alias, with its replacement and removal date.

# The deprecated columns among `read` (table and column pairs), with a note naming the removal date
# and the replacement. Matching on the column as well as the table means a column the lake has
# already removed is never reported, even by a stale copy of `changes`.
deprecated_columns <- function(read, changes) {
  changes |>
    dplyr::semi_join(read, by = c("table", "column")) |>
    dplyr::mutate(
      note = deprecation_note(.data$table, .data$new_table, .data$new_column, .data$removed_on)
    ) |>
    dplyr::select("table", "column", "note")
}

deprecation_note <- function(table, new_table, new_column, removed_on) {
  instead <- dplyr::case_when(
    !is.na(new_column) & new_table == table ~ stringr::str_c("; use `", new_column, "`"),
    !is.na(new_column) ~ stringr::str_c("; use `", new_column, "` on ", new_table),
    !is.na(new_table) ~ stringr::str_c("; see ", new_table),
    .default = ", with no replacement"
  )
  stringr::str_c("deprecated, removed on ", removed_on, instead)
}
