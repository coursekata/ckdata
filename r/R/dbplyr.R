#' @exportS3Method dbplyr::dbplyr_edition
dbplyr_edition.CkConnection <- function(con) 2L

#' @exportS3Method dbplyr::sql_dialect
sql_dialect.CkConnection <- function(con) {
  dbplyr::new_sql_dialect("ckdata", quote_identifier = quote_identifier)
}

date_literal <- function(x) {
  ifelse(
    is.na(x),
    "NULL",
    paste0("DATE ", dbplyr::sql_quote(format(x, "%Y-%m-%d"), "'"))
  )
}

datetime_literal <- function(x) {
  seconds <- round(as.numeric(x), 6)
  whole <- floor(seconds)
  text <- format(.POSIXct(whole, tz = "UTC"), "%Y-%m-%d %H:%M:%S", tz = "UTC")
  # strftime's %OS truncates binary fractions (56.123 can become 56.122999).
  fraction <- sub("^0", "", sub("0+$", "", sprintf("%.6f", seconds - whole)))
  text <- paste0(text, sub("[.]$", "", fraction))
  ifelse(is.na(x), "NULL", paste0("TIMESTAMP ", dbplyr::sql_quote(text, "'")))
}

#' @exportS3Method dbplyr::sql_escape_date
sql_escape_date.sql_dialect_ckdata <- function(con, x) date_literal(x)
#' @exportS3Method dbplyr::sql_escape_datetime
sql_escape_datetime.sql_dialect_ckdata <- function(con, x) datetime_literal(x)

approximation_required <- function(name, replacement) {
  force(name)
  force(replacement)
  function(...) {
    cli::cli_abort(
      "ckdata: {.fun {name}} is not supported remotely; use {.fun {replacement}} to explicitly request an approximation"
    )
  }
}

quantile_translation <- function(window = FALSE) {
  translate <- dbplyr::sql_aggregate_n("APPROX_PERCENTILE")
  function(x, probs = 0.5, na.rm = FALSE) {
    if (is.numeric(probs)) {
      rlang::check_number_decimal(probs, min = 0, max = 1)
    }
    value <- translate(x, probs, na.rm = na.rm)
    if (!window) {
      return(value)
    }
    frame <- dbplyr::win_current_frame()
    dbplyr::win_over(
      value,
      partition = dbplyr::win_current_group(),
      order = if (!rlang::is_null(frame)) dbplyr::win_current_order(),
      frame = frame
    )
  }
}

#' @exportS3Method dbplyr::sql_translation
sql_translation.sql_dialect_ckdata <- function(con) {
  dbplyr::sql_variant(
    scalar = dbplyr::sql_translator(
      .parent = dbplyr::base_scalar,
      as.character = dbplyr::sql_cast("VARCHAR"),
      as.numeric = dbplyr::sql_cast("DOUBLE"),
      as.double = dbplyr::sql_cast("DOUBLE"),
      as.integer64 = dbplyr::sql_cast("BIGINT"),
      is.infinite = function(x) {
        dbplyr::build_sql("COALESCE(IS_INFINITE(", x, "), FALSE)")
      },
      is.finite = function(x) {
        dbplyr::build_sql("COALESCE(IS_FINITE(", x, "), FALSE)")
      },
      is.nan = function(x) {
        dbplyr::build_sql("COALESCE(IS_NAN(", x, "), FALSE)")
      },
      grepl = function(
        pattern,
        x,
        ignore.case = FALSE,
        perl = FALSE,
        fixed = FALSE,
        useBytes = FALSE
      ) {
        purrr::iwalk(
          list(
            ignore.case = ignore.case,
            perl = perl,
            fixed = fixed,
            useBytes = useBytes
          ),
          function(flag, name) {
            rlang::check_bool(flag, arg = name)
          }
        )
        if (perl || useBytes) {
          rlang::abort(
            "ckdata: Athena regex uses Unicode Java patterns; perl and useBytes are unsupported"
          )
        }
        if (
          rlang::is_character(pattern) &&
            length(pattern) == 1L &&
            is.na(pattern)
        ) {
          return(dbplyr::sql("CAST(NULL AS BOOLEAN)"))
        }
        if (fixed) {
          if (ignore.case) {
            x <- dbplyr::build_sql("LOWER(", x, ")")
            pattern <- dbplyr::build_sql("LOWER(", pattern, ")")
          }
          match <- dbplyr::build_sql("STRPOS(", x, ", ", pattern, ") > 0")
        } else {
          if (ignore.case) {
            pattern <- dbplyr::build_sql("CONCAT('(?i)', ", pattern, ")")
          }
          match <- dbplyr::build_sql("REGEXP_LIKE(", x, ", ", pattern, ")")
        }
        dbplyr::build_sql(
          "CASE WHEN ",
          pattern,
          " IS NULL THEN CAST(NULL AS BOOLEAN) ELSE COALESCE(",
          match,
          ", FALSE) END"
        )
      },
      floor_date = function(
        x,
        unit = "seconds",
        week_start = getOption("lubridate.week.start", 7)
      ) {
        unit <- rlang::arg_match0(
          sub("s$", "", unit),
          c(
            "second",
            "minute",
            "hour",
            "day",
            "week",
            "month",
            "quarter",
            "year"
          )
        )
        shift <- 0
        if (unit == "week") {
          rlang::check_number_whole(week_start, min = 1, max = 7)
          shift <- (8 - week_start) %% 7
          if (shift) {
            x <- dbplyr::build_sql(
              "DATE_ADD('day', ",
              as.integer(shift),
              ", ",
              x,
              ")"
            )
          }
        }
        value <- dbplyr::build_sql("DATE_TRUNC(", unit, ", ", x, ")")
        if (shift) {
          value <- dbplyr::build_sql(
            "DATE_ADD('day', ",
            as.integer(-shift),
            ", ",
            value,
            ")"
          )
        }
        value
      },
      month = function(x, label = FALSE, abbr = TRUE) {
        if (!rlang::is_false(label)) {
          rlang::abort("ckdata: month(label = TRUE) is not supported remotely")
        }
        dbplyr::build_sql("MONTH(", x, ")")
      },
      year = dbplyr::sql_prefix("YEAR", 1),
      day = dbplyr::sql_prefix("DAY", 1),
      hour = dbplyr::sql_prefix("HOUR", 1),
      minute = dbplyr::sql_prefix("MINUTE", 1),
      second = dbplyr::sql_prefix("SECOND", 1)
    ),
    aggregate = dbplyr::sql_translator(
      .parent = dbplyr::base_agg,
      median = approximation_required("median", "approx_median"),
      quantile = approximation_required("quantile", "approx_quantile"),
      approx_median = function(x, na.rm = FALSE) {
        quantile_translation()(x, 0.5, na.rm = na.rm)
      },
      approx_quantile = quantile_translation()
    ),
    window = dbplyr::sql_translator(
      .parent = dbplyr::base_win,
      median = approximation_required("median", "approx_median"),
      quantile = approximation_required("quantile", "approx_quantile"),
      approx_median = function(x, na.rm = FALSE) {
        quantile_translation(window = TRUE)(x, 0.5, na.rm = na.rm)
      },
      approx_quantile = quantile_translation(window = TRUE)
    )
  )
}

#' Explicit approximate summaries for remote queries
#'
#' These functions are used inside dbplyr expressions. Athena evaluates
#' `APPROX_PERCENTILE`; they do not compute a local R summary. `probs` is a
#' single probability between zero and one. NULL values follow SQL semantics.
#' @param x A remote numeric column.
#' @param probs One probability, such as `0.95`.
#' @param na.rm Whether to suppress dbplyr's reminder that SQL drops NULLs.
#' @export
approx_quantile <- function(x, probs = 0.5, na.rm = FALSE) {
  rlang::abort("ckdata: approx_quantile() is for remote dbplyr expressions")
}
#' @rdname approx_quantile
#' @export
approx_median <- function(x, na.rm = FALSE) {
  rlang::abort("ckdata: approx_median() is for remote dbplyr expressions")
}

#' @exportS3Method dplyr::tbl
tbl.CkConnection <- function(src, from, ..., vars = NULL) {
  # Known table names use Athena's catalog API. Arbitrary SQL projections use
  # dbplyr's normal field-discovery query because catalog metadata cannot
  # describe expressions, aliases or joins.
  if (rlang::is_null(vars) && !inherits(from, c("SQL", "sql"))) {
    id <- table_id(src, from)
    vars <- DBI::dbListFields(
      src,
      DBI::Id(schema = id$schema, table = id$table)
    )
  }
  dbplyr::tbl_sql("ckdata", dbplyr::src_dbi(src), from = from, vars = vars, ...)
}

#' @exportS3Method dbplyr::db_connection_describe
db_connection_describe.CkConnection <- function(con, ...) {
  info <- DBI::dbGetInfo(con)
  paste0("ckdata/Athena [", info$schema, "; ", info$workgroup, "]")
}

#' @exportS3Method dbplyr::db_compute
db_compute.CkConnection <- function(con, ...) read_only()
#' @exportS3Method dbplyr::db_copy_to
db_copy_to.CkConnection <- function(con, ...) read_only()

# Athena UNLOAD cannot write an unknown-type NULL column. Keep the R type
# while dbplyr still has it; its numeric escape methods otherwise erase it.
# Raw SQL is opaque. This adapter only visits dbplyr's R projection expressions.
typed_missing_expr <- function(x, con, projection = FALSE) {
  if (rlang::is_quosure(x)) {
    return(rlang::quo_set_expr(
      x,
      typed_missing_expr(rlang::quo_get_expr(x), con, projection)
    ))
  }
  if (inherits(x, c("sql", "SQL"))) {
    return(x)
  }
  if (rlang::is_call(x)) {
    outputs <- projection_outputs(x)
    x <- outputs$call
    output <- if (projection && logical_output(x)) {
      outputs$indices
    } else {
      integer()
    }
    # Parentheses are transparent even when their expression needs SQL inference.
    if (projection && rlang::is_call(x, "(")) {
      output <- 2L
    }
    for (i in seq_along(x)[-1L]) {
      if (rlang::is_missing(x[[i]])) {
        next
      }
      value <- x[[i]]
      value <- typed_missing_expr(value, con, projection = i %in% output)
      x[i] <- list(value)
    }
    return(x)
  }
  type <- literal_type(x)
  if (
    rlang::is_null(type) || !anyNA(x) || (rlang::is_logical(x) && !projection)
  ) {
    return(x)
  }
  # Escape elements independently so vector NULLs retain their types.
  values <- purrr::map_chr(seq_along(x), function(i) {
    if (is.na(x[i])) {
      paste0("CAST(NULL AS ", type, ")")
    } else {
      as.character(dbplyr::escape(x[i], con = con))
    }
  })
  dbplyr::sql_vector(
    values,
    parens = length(values) != 1L,
    collapse = ", ",
    con = con
  )
}

# These R calls derive their output type from their value branches. Conditions
# and numeric/column operands retain dbplyr's normal SQL type inference.
projection_outputs <- function(x) {
  name <- rlang::call_name(x)
  if (rlang::is_null(name)) {
    name <- ""
  }
  indices <- integer()
  if (name %in% c("if_else", "ifelse")) {
    x <- match.call(dbplyr::base_scalar[[name]], x)
    indices <- which(names(x) %in% c("true", "false", "missing", "yes", "no"))
  } else if (rlang::is_string(name, "coalesce")) {
    indices <- seq_along(x)[-1L]
  } else if (name %in% c("case_when", "case_match")) {
    if (rlang::is_string(name, "case_match")) {
      x <- match.call(dbplyr::base_scalar$case_match, x)
    }
    indices <- seq_along(x)[-1L]
    indices <- indices[
      !(rlang::names2(x)[indices] %in% c(".x", ".ptype", ".size"))
    ]
  } else if (rlang::is_string(name, "(")) {
    indices <- 2L
  } else if (rlang::is_string(name, "~") && length(x) == 3L) {
    indices <- 3L
  }
  list(call = x, indices = indices)
}

logical_output <- function(x) {
  if (rlang::is_null(x) || rlang::is_logical(x)) {
    return(TRUE)
  }
  if (!rlang::is_call(x)) {
    return(FALSE)
  }
  outputs <- projection_outputs(x)
  length(outputs$indices) > 0L &&
    purrr::every(outputs$indices, function(i) logical_output(outputs$call[[i]]))
}

typed_inline_values <- function(query, con) {
  doubles <- purrr::map_lgl(query$x, function(x) {
    identical(literal_type(x), "DOUBLE")
  })
  if (!any(doubles)) {
    return(query)
  }
  # VALUES resolves column types before dbplyr's outer casts. Encode doubles
  # together so finite values and signed infinity have the same SQL type.
  # Keep the original types before replacing columns with SQL vectors.
  if (rlang::is_null(query$col_types)) {
    query$col_types <- DBI::dbDataType(con, query$x)
  }
  query$x[doubles] <- purrr::map(query$x[doubles], function(x) {
    # Inline NaN follows dbplyr's projected NaN-to-NULL semantics.
    x[is.nan(x)] <- NA_real_
    dbplyr::sql(parameter_literals(con, x))
  })
  query
}

typed_missing_query <- function(query, con) {
  if (inherits(query, "tbl_lazy")) {
    query$lazy_query <- typed_missing_query(query$lazy_query, con)
    return(query)
  }
  if (inherits(query, "lazy_base_values_query")) {
    return(typed_inline_values(query, con))
  }
  if (!rlang::is_list(query)) {
    return(query)
  }
  for (name in seq_along(query)) {
    if (rlang::is_list(query[[name]])) {
      query[[name]] <- typed_missing_query(query[[name]], con)
    }
  }
  if (inherits(query, "lazy_select_query")) {
    query$select$expr <- purrr::map(
      query$select$expr,
      typed_missing_expr,
      con = con,
      projection = TRUE
    )
  }
  query
}

#' @exportS3Method dbplyr::db_sql_render
db_sql_render.CkConnection <- function(
  con,
  sql,
  ...,
  cte = FALSE,
  sql_options = NULL
) {
  dbplyr::sql_render(
    typed_missing_query(sql, con),
    con = con,
    ...,
    sql_options = sql_options
  )
}
