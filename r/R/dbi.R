#' CourseKata's read-only DBI driver
#'
#' Connect with `DBI::dbConnect(ckdata::ckdata(), workgroup = ..., schema = ...)`.
#' Credentials and region follow the standard AWS SDK configuration. Supplying
#' `profile` or `region` overrides discovery for this connection. An explicit
#' `role_arn` is assumed once, for at most one hour, and is never renewed.
#'
#' @return `ckdata()` returns a DBI driver. The methods return the values
#'   described by their corresponding DBI generics.
#' @export
ckdata <- function() methods::new("CkDriver")

#' @rdname ckdata
#' @export
methods::setClass("CkDriver", contains = "DBIDriver")
#' @rdname ckdata
#' @export
methods::setClass("CkConnection", contains = "DBIConnection", slots = c(state = "environment"))
#' @rdname ckdata
#' @export
methods::setClass("CkResult", contains = "DBIResult", slots = c(state = "environment"))
#' @rdname ckdata
#' @export
methods::setClass("CkResultArrow", contains = c("CkResult", "DBIResultArrow"))

check_connection <- function(conn) {
  if (!rlang::is_true(conn@state$open)) rlang::abort("ckdata: connection is closed")
  info <- native_connection_info(conn@state$handle)
  if (nzchar(info$session_error)) rlang::abort(info$session_error)
  if (rlang::is_true(info$expiring)) {
    warn_session("assumed-role session expires within fifteen minutes and will not renew")
  }
  invisible(info)
}

check_result <- function(res) {
  if (!rlang::is_true(res@state$open) || !rlang::is_true(res@state$conn@state$open)) {
    rlang::abort("ckdata: result is closed")
  }
  invisible(NULL)
}

single_string <- function(x, name, call = rlang::caller_env()) {
  rlang::check_string(x, allow_empty = FALSE, arg = name, call = call)
  rlang::check_string(trimws(x), allow_empty = FALSE, arg = name, call = call)
  x
}

#' @rdname ckdata
#' @param drv A driver returned by `ckdata()`.
#' @param workgroup Athena workgroup, or `CK_DATALAKE_WORKGROUP`.
#' @param schema Default database, or [ckdata_schema()].
#' @param profile,region Optional explicit AWS profile and region.
#' @param role_arn Optional role to assume once; defaults to `CK_DATALAKE_ROLE_ARN`.
#' @param freshness Optional default policy for [collect_saved()] on this
#'   connection. NULL inherits application defaults at collection time.
#' @param ... Additional arguments; unsupported arguments raise an error.
#' @export
methods::setMethod("dbConnect", "CkDriver", function(
  drv,
  workgroup = getOption("ckdata.workgroup", Sys.getenv("CK_DATALAKE_WORKGROUP")),
  schema = ckdata_schema(), profile = NULL, region = NULL,
  role_arn = lake_env("CK_DATALAKE_ROLE_ARN", required = FALSE), freshness = NULL, ...
) {
  rlang::check_dots_empty()
  check_freshness(freshness)
  workgroup <- single_string(workgroup, "workgroup")
  schema <- single_string(schema, "schema")
  purrr::iwalk(list(profile = profile, region = region, role_arn = role_arn), function(value, name) {
    if (!rlang::is_null(value)) single_string(value, name)
  })
  state <- rlang::new_environment(list(
    handle = native_wait(native_connect(workgroup, schema, profile, region, role_arn)),
    open = TRUE, results = rlang::new_environment(), next_result = 0L, freshness = freshness
  ))
  methods::new("CkConnection", state = state)
})

#' @rdname ckdata
#' @param conn A ckdata DBI connection.
#' @export
methods::setMethod("dbDisconnect", "CkConnection", function(conn, ...) {
  rlang::check_dots_empty()
  if (!rlang::is_true(conn@state$open)) {
    rlang::warn("ckdata: connection is already closed")
    return(invisible(TRUE))
  }
  results <- rlang::env_get_list(conn@state$results, rlang::env_names(conn@state$results))
  if (length(results)) rlang::warn("ckdata: closing connection with uncleared results")
  for (result in results) {
    if (!rlang::is_null(result$handle)) native_result_clear(result$handle)
    result$open <- FALSE
  }
  rlang::env_unbind(conn@state$results, rlang::env_names(conn@state$results))
  native_disconnect(conn@state$handle)
  conn@state$open <- FALSE
  invisible(TRUE)
})

#' @rdname ckdata
#' @param dbObj A ckdata driver, connection, or result.
#' @export
methods::setMethod("dbIsValid", "CkDriver", function(dbObj, ...) TRUE)
#' @rdname ckdata
#' @export
methods::setMethod("dbIsValid", "CkConnection", function(dbObj, ...) {
  if (!rlang::is_true(dbObj@state$open)) {
    return(FALSE)
  }
  rlang::try_fetch(
    {
      info <- native_connection_info(dbObj@state$handle)
      rlang::is_true(info$open) && !nzchar(info$session_error)
    },
    error = function(e) FALSE
  )
})
#' @rdname ckdata
#' @export
methods::setMethod("dbIsValid", "CkResult", function(dbObj, ...) {
  if (!rlang::is_true(dbObj@state$open) || !rlang::is_true(dbObj@state$conn@state$open)) {
    return(FALSE)
  }
  if (rlang::is_null(dbObj@state$handle)) {
    return(TRUE)
  }
  rlang::try_fetch(rlang::is_true(native_result_info(dbObj@state$handle)$open), error = function(e) FALSE)
})
#' @rdname ckdata
#' @export
methods::setMethod("dbGetInfo", "CkConnection", function(dbObj, ...) {
  info <- native_connection_info(dbObj@state$handle)
  c(list(
    dbname = info$schema, db.version = NA_character_, username = info$arn,
    host = NA_character_, port = NA_integer_,
    driver.version = as.character(utils::packageVersion("ckdata"))
  ), info)
})

#' @exportS3Method base::format
format.CkConnection <- function(x, ...) {
  if (!rlang::is_true(x@state$open)) {
    return("<ckdata connection: closed>")
  }
  info <- rlang::try_fetch(DBI::dbGetInfo(x), error = function(e) NULL)
  if (rlang::is_null(info)) {
    return("<ckdata connection: invalid; reconnect>")
  }
  paste0("<ckdata connection: ", info$schema, " [", info$workgroup, "]>")
}
#' @rdname ckdata
#' @export
#' @param object A ckdata connection to display.
methods::setMethod("show", "CkConnection", function(object) cat(format(object), "\n"))

new_result <- function(conn, statement, value = NULL, arrow = FALSE, parameter_count = 0L) {
  # If warn = 2 promotes a warning to an error, no unreachable result may
  # remain registered on the connection.
  warn_session(value$warnings)
  state <- rlang::new_environment(list(
    conn = conn, handle = value$handle, parameter_count = parameter_count,
    statement = statement, query_id = value$query_id, open = TRUE
  ))
  conn@state$next_result <- conn@state$next_result + 1L
  state$id <- as.character(conn@state$next_result)
  rlang::env_poke(conn@state$results, state$id, state)
  methods::new(if (arrow) "CkResultArrow" else "CkResult", state = state)
}

send_query <- function(conn, statement, params = NULL, immediate = NULL, arrow = FALSE, ...) {
  rlang::check_dots_empty()
  check_connection(conn)
  statement <- single_string(statement, "statement")
  rlang::check_bool(immediate, allow_null = TRUE)
  # DBI 1.3 skips the first character after a comment opener. Pad only the
  # analysis copy so empty comments and an immediate newline remain visible.
  analysis <- gsub("/*", "/* ", statement, fixed = TRUE)
  analysis <- gsub("--", "-- ", analysis, fixed = TRUE)
  variables <- DBI::sqlParseVariables(conn, analysis)
  if (length(variables$start) && any(substring(analysis, variables$start, variables$end) != "?")) {
    rlang::abort("ckdata: Athena uses positional ? placeholders")
  }
  count <- length(variables$start)
  if (!count && rlang::is_null(params)) {
    value <- run_query(conn, statement)
    return(new_result(conn, statement, value, arrow))
  }
  result <- new_result(conn, statement, arrow = arrow, parameter_count = count)
  success <- FALSE
  withr::defer(if (!success) DBI::dbClearResult(result))
  if (!rlang::is_null(params)) DBI::dbBind(result, params)
  success <- TRUE
  result
}

#' @rdname ckdata
#' @param statement SQL query.
#' @param params An unnamed list of equal-length parameter vectors for Athena's
#'   positional `?` placeholders. Data frame columns bind by position. Values are
#'   sent separately using Athena execution parameters. Each row executes once;
#'   results are concatenated. For statements with placeholders and `params =
#'   NULL`, call `dbBind()` before fetching. For `dbBindArrow()`, supply an Arrow
#'   stream with one column per placeholder. Arrow integer and decimal columns
#'   retain their precision before R conversion, within Athena's supported types.
#' @param immediate Accepted for DBI compatibility; Athena has one query API.
#' @export
methods::setMethod("dbSendQuery", c("CkConnection", "character"), function(conn, statement, ..., params = NULL, immediate = NULL) {
  send_query(conn, statement, params, immediate, ...)
})
#' @rdname ckdata
#' @export
methods::setMethod("dbSendQueryArrow", c("CkConnection", "character"), function(conn, statement, ..., params = NULL, immediate = NULL) {
  send_query(conn, statement, params, immediate, arrow = TRUE, ...)
})

#' @rdname ckdata
#' @export
methods::setMethod("dbGetQuery", c("CkConnection", "character"), function(conn, statement, ..., n = -1, params = NULL, immediate = NULL) {
  result <- DBI::dbSendQuery(conn, statement, params = params, immediate = immediate)
  withr::defer(DBI::dbClearResult(result))
  DBI::dbFetch(result, n = n, ...)
})

#' @rdname ckdata
#' @export
methods::setMethod("dbGetQueryArrow", c("CkConnection", "character"), function(conn, statement, ..., params = NULL, immediate = NULL) {
  result <- DBI::dbSendQueryArrow(conn, statement, ..., params = params, immediate = immediate)
  withr::defer(DBI::dbClearResult(result))
  DBI::dbFetchArrow(result)
})

#' @rdname ckdata
#' @export
methods::setMethod("dbGetInfo", "CkResultArrow", function(dbObj, ...) {
  DBI::dbGetInfo(methods::as(dbObj, "CkResult"), ...)
})

#' @rdname ckdata
#' @export
methods::setMethod("dbFetchArrowChunk", "CkResultArrow", function(res, ...) {
  rlang::check_dots_empty()
  check_bound_result(res)
  stream <- nanoarrow::as_nanoarrow_array_stream(native_result_fetch(res@state$handle, 256))
  table <- arrow::as_arrow_table(stream)
  nanoarrow::as_nanoarrow_array(arrow::as_record_batch(table, schema = table$schema))
})

stream_frame <- function(stream) {
  table_frame(arrow::as_arrow_table(nanoarrow::as_nanoarrow_array_stream(stream)))
}

table_frame <- function(table) {
  # Use Arrow's established nested conversion. Keep integer64 columns stable
  # across empty/populated results instead of downcasting based on values.
  withr::with_options(list(arrow.int64_downcast = FALSE), as.data.frame(table))
}

#' @rdname ckdata
#' @param res A ckdata DBI result.
#' @param n Maximum rows to fetch; `-1` or `Inf` fetches all remaining rows.
#' @param row.names `FALSE` or `NULL`; columns are not converted into row names.
#' @export
methods::setMethod("dbFetch", "CkResult", function(res, n = -1, ..., row.names = FALSE) {
  rlang::check_dots_empty()
  check_bound_result(res)
  if (!rlang::is_null(row.names) && !rlang::is_false(row.names)) {
    rlang::abort("ckdata: row.names must be FALSE or NULL; result columns are ordinary columns")
  }
  if (!(is.numeric(n) || rlang::is_lgl_na(n)) || length(n) != 1L || is.nan(n) || (!is.na(n) && !(n == -1 || n == Inf || (is.finite(n) && n >= 0 && n == floor(n))))) {
    rlang::abort("ckdata: n must be a non-negative whole number, -1, NA or Inf")
  }
  stream_frame(native_result_fetch(res@state$handle, if (is.na(n)) -1 else as.numeric(n)))
})
#' @rdname ckdata
#' @export
methods::setMethod("dbFetchArrow", "CkResultArrow", function(res, ...) {
  rlang::check_dots_empty()
  check_bound_result(res)
  nanoarrow::as_nanoarrow_array_stream(native_result_fetch(res@state$handle, -1))
})
#' @rdname ckdata
#' @export
methods::setMethod("dbClearResult", "CkResult", function(res, ...) {
  rlang::check_dots_empty()
  if (!rlang::is_true(res@state$open)) {
    rlang::warn("ckdata: result is already cleared")
    return(invisible(TRUE))
  }
  if (!rlang::is_null(res@state$handle)) native_result_clear(res@state$handle)
  res@state$open <- FALSE
  rlang::env_unbind(res@state$conn@state$results, res@state$id)
  invisible(TRUE)
})
#' @rdname ckdata
#' @export
methods::setMethod("dbHasCompleted", "CkResult", function(res, ...) {
  check_result(res)
  if (rlang::is_null(res@state$handle)) {
    return(FALSE)
  }
  info <- native_result_info(res@state$handle)
  info$fetched == info$rows
})
#' @rdname ckdata
#' @export
methods::setMethod("dbGetRowCount", "CkResult", function(res, ...) {
  check_result(res)
  if (rlang::is_null(res@state$handle)) {
    return(0L)
  }
  native_result_info(res@state$handle)$fetched
})
#' @rdname ckdata
#' @export
methods::setMethod("dbGetRowsAffected", "CkResult", function(res, ...) {
  check_result(res)
  0L
})
#' @rdname ckdata
#' @export
methods::setMethod("dbGetStatement", "CkResult", function(res, ...) {
  check_result(res)
  res@state$statement
})
#' @rdname ckdata
#' @export
methods::setMethod("dbColumnInfo", "CkResult", function(res, ...) {
  check_bound_result(res)
  info <- native_result_info(res@state$handle)
  data.frame(name = info$name, type = info$data_type)
})

quote_identifier <- function(x) dbplyr::sql_quote(x, '"')
#' @rdname ckdata
#' @param x Names or values to quote.
#' @export
methods::setMethod("dbQuoteIdentifier", c("CkConnection", "character"), function(conn, x, ...) {
  if (anyNA(x)) rlang::abort("ckdata: identifiers cannot contain NA")
  DBI::SQL(as.character(quote_identifier(x)), names = names(x))
})
#' @rdname ckdata
#' @export
methods::setMethod("dbQuoteIdentifier", c("CkConnection", "SQL"), function(conn, x, ...) x)
#' @rdname ckdata
#' @export
methods::setMethod("dbQuoteString", c("CkConnection", "character"), function(conn, x, ...) {
  DBI::SQL(ifelse(is.na(x), "NULL", paste0("'", gsub("'", "''", x, fixed = TRUE), "'")))
})
#' @rdname ckdata
#' @export
methods::setMethod("dbQuoteString", c("CkConnection", "SQL"), function(conn, x, ...) x)

#' @rdname ckdata
#' @export
methods::setMethod("dbQuoteLiteral", c("CkConnection", "ANY"), function(conn, x, ...) {
  if (inherits(x, "SQL") || rlang::is_null(literal_type(x))) {
    return(methods::callNextMethod())
  }
  DBI::SQL(parameter_literals(conn, x), names = names(x))
})

table_id <- function(conn, name) {
  schema <- native_connection_info(conn@state$handle)$schema
  if (inherits(name, "SQL")) {
    single_string(as.character(name), "name")
    name <- DBI::dbUnquoteIdentifier(conn, name)[[1L]]
  }
  path <- dbplyr::as_table_path(name, conn)
  parts <- dbplyr::table_path_components(path, conn)[[1]]
  if (length(parts) == 3L && parts[[1]] != "AwsDataCatalog") rlang::abort("ckdata: only AwsDataCatalog is supported")
  if (length(parts) < 1L || length(parts) > 3L) rlang::abort("ckdata: invalid table path")
  if (length(parts) >= 2L) schema <- parts[[length(parts) - 1L]]
  name <- parts[[length(parts)]]
  list(schema = single_string(schema, "schema"), table = single_string(name, "table"))
}

ck_fields <- function(conn, name, ...) {
  rlang::check_dots_empty()
  check_connection(conn)
  id <- table_id(conn, name)
  native_wait(native_metadata(conn@state$handle, id$schema, id$table))$name
}
#' @rdname ckdata
#' @param name A table name or `DBI::Id(schema = ..., table = ...)`.
#' @export
methods::setMethod("dbListFields", c("CkConnection", "character"), ck_fields)
#' @rdname ckdata
#' @export
methods::setMethod("dbListFields", c("CkConnection", "Id"), ck_fields)
#' @rdname ckdata
#' @export
methods::setMethod("dbListTables", "CkConnection", function(conn, ...) {
  rlang::check_dots_empty()
  check_connection(conn)
  native_wait(native_metadata(conn@state$handle, native_connection_info(conn@state$handle)$schema, NULL))
})
ck_exists <- function(conn, name, ...) {
  rlang::check_dots_empty()
  check_connection(conn)
  id <- table_id(conn, name)
  id$table %in% native_wait(native_metadata(conn@state$handle, id$schema, NULL))
}
#' @rdname ckdata
#' @export
methods::setMethod("dbExistsTable", c("CkConnection", "character"), ck_exists)
#' @rdname ckdata
#' @export
methods::setMethod("dbExistsTable", c("CkConnection", "Id"), ck_exists)
# dbReadTable/dbReadTableArrow use DBI's defaults, including row/name handling.

#' @rdname ckdata
#' @param prefix Optional schema or table path for catalog discovery.
#' @export
methods::setMethod("dbListObjects", "CkConnection", function(conn, prefix = NULL, ...) {
  rlang::check_dots_empty()
  check_connection(conn)
  schema <- native_connection_info(conn@state$handle)$schema
  parts <- character()
  if (!rlang::is_null(prefix)) {
    ids <- DBI::dbUnquoteIdentifier(conn, prefix)
    if (length(ids) != 1L) rlang::abort("ckdata: prefix must be one identifier")
    parts <- ids[[1L]]@name
    # SQL paths have positional components; a one-part prefix names a schema.
    if (!inherits(prefix, "Id") && length(parts)) {
      if (length(parts) > 3L) rlang::abort("ckdata: invalid catalog prefix")
      names(parts) <- if (parts[[1L]] == "AwsDataCatalog") {
        c("catalog", "schema", "table")[seq_along(parts)]
      } else {
        switch(length(parts),
          "schema",
          c("schema", "table"),
          c("catalog", "schema", "table")
        )
      }
    }
    if (any(!names(parts) %in% c("catalog", "schema", "table"))) rlang::abort("ckdata: invalid catalog prefix")
    if ("catalog" %in% names(parts) && parts[["catalog"]] != "AwsDataCatalog") rlang::abort("ckdata: only AwsDataCatalog is supported")
    for (name in names(parts)) single_string(parts[[name]], "prefix")
    if ("schema" %in% names(parts)) schema <- parts[["schema"]]
    if ("catalog" %in% names(parts) && !"schema" %in% names(parts)) parts[["schema"]] <- schema
  }
  tables <- native_wait(native_metadata(conn@state$handle, schema, NULL))
  if ("table" %in% names(parts)) tables <- intersect(tables, parts[["table"]])
  ids <- purrr::map(tables, function(table) {
    fields <- as.list(parts[intersect(names(parts), c("catalog", "schema"))])
    rlang::exec(DBI::Id, !!!fields, table = table)
  })
  data.frame(table = I(unname(ids)), is_prefix = rep(FALSE, length(ids)))
})

read_only <- function(...) rlang::abort("ckdata: this operation is not supported by the read-only query interface")
#' @rdname ckdata
#' @export
methods::setMethod("dbSendStatement", c("CkConnection", "character"), function(conn, statement, ...) read_only())
#' @rdname ckdata
#' @export
methods::setMethod("dbBegin", "CkConnection", function(conn, ...) read_only())
#' @rdname ckdata
#' @export
methods::setMethod("dbCommit", "CkConnection", function(conn, ...) read_only())
#' @rdname ckdata
#' @export
methods::setMethod("dbRollback", "CkConnection", function(conn, ...) read_only())
#' @rdname ckdata
#' @export
methods::setMethod("dbBind", "CkResult", function(res, params, ...) {
  rlang::check_dots_empty()
  check_result(res)
  check_connection(res@state$conn)
  count <- res@state$parameter_count
  if (!count) rlang::abort("ckdata: query has no parameter placeholders")
  if (!rlang::is_list(params) || length(params) != count) {
    rlang::abort("ckdata: params must have one vector per placeholder")
  }
  if (!is.data.frame(params) && !rlang::is_null(names(params))) {
    rlang::abort("ckdata: positional parameters must be unnamed")
  }
  sizes <- lengths(params)
  if (length(unique(sizes)) != 1L) rlang::abort("ckdata: parameter vectors must have equal lengths")
  if (purrr::some(params, is.factor)) rlang::warn("ckdata: factor parameters are converted to character")
  # Encode every value before submitting anything. Never splice values into SQL.
  encoded <- purrr::map(params, function(x) parameter_literals(res@state$conn, x))
  empty <- if (!sizes[[1L]]) {
    purrr::map_chr(params, function(x) parameter_literals(res@state$conn, x[NA_integer_]))
  } else {
    character()
  }
  bind_encoded_rows(res, encoded, empty)
})

bind_encoded_rows <- function(res, encoded, empty) {
  size <- length(encoded[[1L]])
  staged <- NULL
  withr::defer(if (!rlang::is_null(staged)) native_result_clear(staged))
  for (i in seq_len(max(1L, size))) {
    values <- if (size) purrr::map_chr(encoded, i) else empty
    value <- run_query(res@state$conn, res@state$statement, unname(values), schema_only = !size)
    incoming <- value$handle
    rlang::try_fetch(
      {
        if (rlang::is_null(staged)) staged <- incoming else native_result_append(staged, incoming)
      },
      error = function(e) {
        native_result_clear(incoming)
        rlang::cnd_signal(e)
      }
    )
  }
  # Publish and transfer ownership before releasing the previous cursor. Only
  # this short commit is protected; query execution remains interruptible.
  suspendInterrupts({
    previous <- res@state$handle
    res@state$handle <- staged
    res@state$query_id <- value$query_id
    staged <- NULL
    if (!rlang::is_null(previous)) native_result_clear(previous)
  })
  invisible(res)
}

#' @rdname ckdata
#' @export
methods::setMethod("dbBindArrow", "CkResult", function(res, params, ...) {
  rlang::check_dots_empty()
  check_result(res)
  check_connection(res@state$conn)
  count <- res@state$parameter_count
  if (!count) rlang::abort("ckdata: query has no parameter placeholders")
  table <- arrow::as_arrow_table(params)
  if (table$num_columns != count) rlang::abort("ckdata: params must have one vector per placeholder")
  prepared <- purrr::map(seq_len(count), function(i) arrow_parameter_literals(res@state$conn, table[[i]]))
  bind_encoded_rows(res, purrr::map(prepared, "values"), purrr::map_chr(prepared, "empty"))
})

arrow_parameter_literals <- function(conn, column) {
  type <- column$type
  sql_type <- switch(type$ToString(),
    int8 = "TINYINT",
    int16 = "SMALLINT",
    int32 = "INTEGER",
    int64 = "BIGINT",
    uint8 = "SMALLINT",
    uint16 = "INTEGER",
    uint32 = "BIGINT",
    uint64 = "DECIMAL(20, 0)",
    NULL
  )
  if (inherits(type, "DecimalType")) {
    precision <- type$precision()
    scale <- type$scale()
    if (precision > 38L || scale < 0L || scale > precision) {
      cli::cli_abort("ckdata: unsupported Arrow parameter type: {type$ToString()}")
    }
    sql_type <- paste0("DECIMAL(", precision, ", ", scale, ")")
  }
  if (!rlang::is_null(sql_type)) {
    # Arrow's string cast preserves the full integer/decimal range and its NULL
    # bitmap, including integer minima that R reserves as missing-value sentinels.
    text <- as.vector(column$cast(arrow::utf8()))
    text <- as.character(DBI::dbQuoteString(conn, text))
    return(list(
      values = if (length(text)) paste0("CAST(", text, " AS ", sql_type, ")") else character(),
      empty = paste0("CAST(NULL AS ", sql_type, ")")
    ))
  }
  value <- table_frame(arrow::Table$create(value = column))[[1L]]
  if (is.factor(value)) rlang::warn("ckdata: factor parameters are converted to character")
  list(
    values = parameter_literals(conn, value),
    empty = parameter_literals(conn, value[NA_integer_])
  )
}

#' @rdname ckdata
#' @export
methods::setMethod("dbGetInfo", "CkDriver", function(dbObj, ...) {
  list(
    driver.version = as.character(utils::packageVersion("ckdata")),
    client.version = core_versions()[1]
  )
})
#' @rdname ckdata
#' @param obj An R object for SQL type description.
#' @export
methods::setMethod("dbDataType", "CkDriver", function(dbObj, obj, ...) {
  if (is.data.frame(obj)) {
    return(purrr::map_chr(obj, function(x) DBI::dbDataType(dbObj, x)))
  }
  type <- literal_type(obj)
  if (!rlang::is_null(type)) {
    return(type)
  }
  if (rlang::is_raw(obj) || (rlang::is_list(obj) && purrr::every(obj, function(x) rlang::is_null(x) || rlang::is_raw(x)))) {
    return("VARBINARY")
  }
  DBI::dbDataType(DBI::ANSI(), obj, ...)
})
#' @rdname ckdata
#' @export
methods::setMethod("dbDataType", "CkConnection", function(dbObj, obj, ...) DBI::dbDataType(ckdata(), obj, ...))
#' @rdname ckdata
#' @export
methods::setMethod("dbUnloadDriver", "CkDriver", function(drv, ...) invisible(TRUE))

#' @rdname ckdata
#' @export
methods::setMethod("dbIsReadOnly", "CkDriver", function(dbObj, ...) TRUE)
#' @rdname ckdata
#' @export
methods::setMethod("dbIsReadOnly", "CkConnection", function(dbObj, ...) TRUE)
#' @rdname ckdata
#' @export
methods::setMethod("dbIsReadOnly", "CkResult", function(dbObj, ...) TRUE)

check_bound_result <- function(res) {
  check_result(res)
  if (rlang::is_null(res@state$handle)) rlang::abort("ckdata: bind query parameters before fetching")
}

warn_session <- function(messages) {
  purrr::walk(messages, function(message) {
    rlang::warn(paste0("ckdata: ", message), class = "ckdata_session_warning")
  })
}

run_query <- function(conn, statement, parameters = character(), schema_only = FALSE) {
  value <- native_wait(native_query(
    conn@state$handle, statement,
    getOption("ckdata.timeout", 1800), parameters, schema_only
  ))
  success <- FALSE
  withr::defer(if (!success) native_result_clear(value$handle))
  warn_session(value$warnings)
  value$warnings <- character()
  success <- TRUE
  value
}

parameter_literals <- function(conn, x) {
  if (rlang::is_list(x) && !inherits(x, "POSIXlt")) {
    if (!purrr::every(x, function(value) rlang::is_null(value) || rlang::is_raw(value))) {
      rlang::abort("ckdata: binary parameters must contain raw vectors or NULL")
    }
    text <- as.character(DBI::dbQuoteLiteral(conn, x))
    text[purrr::map_lgl(x, rlang::is_null)] <- "CAST(NULL AS VARBINARY)"
    return(unname(text))
  }
  type <- literal_type(x)
  if (rlang::is_null(type) || inherits(x, "SQL")) cli::cli_abort("ckdata: unsupported parameter type: {.cls {class(x)}}")
  if (!length(x)) {
    return(character())
  }
  text <- if (inherits(x, "difftime")) {
    paste0("TIME ", DBI::dbQuoteLiteral(DBI::ANSI(), x))
  } else if (inherits(x, "Date")) {
    date_literal(x)
  } else if (inherits(x, "POSIXt")) {
    datetime_literal(as.POSIXct(x))
  } else if (is.factor(x) || rlang::is_character(x)) {
    as.character(DBI::dbQuoteString(conn, as.character(x)))
  } else if (rlang::is_logical(x)) {
    ifelse(x, "TRUE", "FALSE")
  } else if (bit64::is.integer64(x)) {
    paste0("CAST(", as.character(x), " AS BIGINT)")
  } else if (rlang::is_integer(x)) {
    as.character(x)
  } else {
    value <- sprintf("%.17g", x)
    value[is.infinite(x)] <- ifelse(x[is.infinite(x)] > 0, "'Infinity'", "'-Infinity'")
    value[is.nan(x)] <- "'NaN'"
    paste0("CAST(", value, " AS DOUBLE)")
  }
  missing <- is.na(x)
  if (type == "DOUBLE") missing <- missing & !is.nan(x)
  text[missing] <- paste0("CAST(NULL AS ", type, ")")
  unname(text)
}

literal_type <- function(x) {
  if (inherits(x, "AsIs")) class(x) <- setdiff(class(x), "AsIs")
  if (bit64::is.integer64(x)) {
    return("BIGINT")
  }
  if (inherits(x, "difftime")) {
    return("TIME")
  }
  if (inherits(x, "Date")) {
    return("DATE")
  }
  if (inherits(x, "POSIXt")) {
    return("TIMESTAMP")
  }
  if (is.factor(x) || rlang::is_character(x)) {
    return("VARCHAR")
  }
  if (is.object(x)) {
    return(NULL)
  }
  if (rlang::is_logical(x)) {
    return("BOOLEAN")
  }
  if (rlang::is_integer(x)) {
    return("INTEGER")
  }
  if (rlang::is_double(x)) {
    return("DOUBLE")
  }
  NULL
}
