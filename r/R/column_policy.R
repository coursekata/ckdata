# The column check. Every function here works only on its arguments; apply_column_policy()'s
# defaults are the one place it reads session state: the registered tables and the options.

# Report, and in publish mode remove, the flagged columns query `x` uses or returns (see
# ?ckdata_options), given the registered lake `tables`. Reads only the query's SQL and columns, so
# nothing runs against the lake.
apply_column_policy <- function(x,
                                tables = lake_state$tables,
                                mode = ckdata_mode(),
                                allow = ckdata_allow(),
                                changes = column_changes) {
  if (mode == "explore") {
    return(x)
  }
  hits <- flagged_in_query(x, tables, changes, allow)
  if (nrow(hits) == 0) {
    return(x)
  }
  removed <- if (mode == "publish") unique(hits$column[hits$returned]) else character()
  report_flagged(hits, mode, removed, nothing_left = all(colnames(x) %in% removed))
  if (length(removed) == 0) x else dplyr::select(x, -dplyr::all_of(removed))
}

# The flagged columns of the lake tables `x` reads that its SQL uses (`used`) or that it returns
# (`returned`), one row each. A name the SQL defines with `AS` is the query's own column, such as a
# migration shim mutate(lrn_type = question_type), so defining it is not a use.
flagged_in_query <- function(x, tables, changes, allow) {
  con <- dbplyr::remote_con(x)
  sql <- dbplyr::remote_query(x)
  flagged_columns(columns_read(sql, tables), changes, allow) |>
    dplyr::mutate(
      quoted = as.character(
        dbplyr::escape(dbplyr::ident(.data$column), con = con, collapse = NULL)
      ),
      mentions = stringr::str_count(sql, stringr::fixed(.data$quoted)),
      definitions = stringr::str_count(sql, stringr::fixed(stringr::str_c("AS ", .data$quoted))),
      used = .data$mentions > .data$definitions,
      returned = .data$column %in% colnames(x) & .data$definitions == 0
    ) |>
    dplyr::filter(.data$used | .data$returned)
}

# The table and column pairs of the registered `tables` that `sql` reads: a table is read when its
# quoted path appears in the SQL.
columns_read <- function(sql, tables) {
  tables |>
    dplyr::filter(stringr::str_detect(sql, stringr::fixed(.data$path))) |>
    dplyr::select("table", "column")
}

# The flagged columns among `read`, less `allow`, each with a note saying why.
flagged_columns <- function(read, changes, allow) {
  dplyr::bind_rows(deprecated_columns(read, changes), internal_columns(read)) |>
    dplyr::filter(!is_allowed(.data$table, .data$column, allow))
}

# The columns among `read` that only a staging or intermediate table provides. A column a published
# table in the query also has, such as a join key, is left out.
internal_columns <- function(read) {
  internal <- stringr::str_detect(read$table, "^(stg|int)_")
  read[internal, ] |>
    dplyr::filter(!.data$column %in% read$column[!internal]) |>
    dplyr::mutate(note = stringr::str_c(
      dplyr::if_else(stringr::str_starts(.data$table, "stg_"), "staging", "intermediate"),
      " table, may change without notice"
    ))
}

is_allowed <- function(table, column, allow) {
  purrr::map2_lgl(table, column, \(table, column) column %in% allow[[table]])
}

# Warn about the flagged columns, or fail when publish mode would leave the query nothing to return:
# one bullet per column, then what publish mode did, then how to keep a column.
report_flagged <- function(hits, mode, removed, nothing_left) {
  outcome <- if (nothing_left) {
    "Every column the query returns is flagged, so it has nothing left to return."
  } else if (length(removed) > 0) {
    cli::format_inline("Removed from the result: {.code {removed}}.")
  }
  message <- c(
    cli::format_inline(
      "ckdata ({mode} mode): {dplyr::n_distinct(hits$column)} column{?s} in this query ",
      "{?is/are} deprecated or may change."
    ),
    rlang::set_names(stringr::str_glue_data(hits, "`{column}` ({table}): {note}."), "*"),
    x = outcome,
    i = cli::format_inline(
      "Keep a column with ",
      '{.code options(ckdata.allow = list({hits$table[[1]]} = "{hits$column[[1]]}"))}.'
    )
  )
  if (nothing_left) {
    rlang::abort(message, class = "ckdata_flagged_columns", call = NULL)
  }
  rlang::warn(message, class = "ckdata_flagged_columns")
}
