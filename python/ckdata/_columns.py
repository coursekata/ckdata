"""Inspect Ibis queries and apply the shared core's column-policy decision."""

from __future__ import annotations

import warnings

import ibis.expr.operations as ops
import sqlglot as sg

from . import _native
from ._errors import ColumnPolicyError, ColumnWarning
from ._settings import current_options


def _columns_read(query, backend, sql):
    read = set()
    for table in query.op().find(ops.DatabaseTable):
        if table.source is not backend:
            continue
        path = sg.table(
            table.name,
            db=table.namespace.database,
            catalog=table.namespace.catalog,
            quoted=True,
        ).sql(backend.dialect)
        if path in sql:
            read.update((table.name, name) for name in table.schema.names)
    return read


def apply_column_policy(query, backend, *, params=None):
    settings = current_options()
    mode, allow = settings.get("mode", "explore"), settings.get("allow", {})
    if mode == "explore":
        return query
    sql = backend.compile(query, params=params)
    read = []
    for table, column in sorted(_columns_read(query, backend, sql)):
        quoted = sg.to_identifier(column, quoted=True).sql(backend.dialect)
        definitions = sql.count(f"AS {quoted}")
        returned = column in query.columns and definitions == 0
        read.append((table, column, sql.count(quoted) > definitions, returned))
    hits, removed, nothing_left = _native._column_policy(
        mode,
        read,
        list(query.columns),
        {table: list(columns) for table, columns in allow.items()},
    )
    if not hits:
        return query
    message = (
        f"ckdata ({mode} mode): deprecated or unstable columns in this query:\n"
        + "\n".join(
            f"`{column}` ({table}): {note}." for table, column, note, *_ in hits
        )
    )
    if nothing_left:
        raise ColumnPolicyError(
            message
            + "\nEvery output column is flagged; explicitly allow columns to retain them."
        )
    if removed:
        message += "\nRemoved from the result: " + ", ".join(sorted(removed)) + "."
    message += "\nKeep a column with configure(allow={'table': ['column']})."
    warnings.warn(message, ColumnWarning, stacklevel=3)
    return query.drop(*sorted(removed)) if removed else query
