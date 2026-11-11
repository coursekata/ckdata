"""Ibis owns query construction; the shared Rust core owns AWS execution."""

from __future__ import annotations

import re
import weakref

import ibis
import ibis.expr.operations as ops
import ibis.expr.schema as sch
import pyarrow as pa
import sqlglot as sg
import sqlglot.expressions as sge
from ibis.backends.sql import SQLBackend
from ibis.backends.sql.compilers.athena import AthenaCompiler

from ._freshness import check_freshness


class _Compiler(AthenaCompiler):
    def visit_InMemoryTable(self, op, *, name, schema, data):
        # Small lookup tables stay in VALUES; no staging table or write API.
        rows = data.to_pyarrow(schema).to_pylist()
        values = [
            tuple(
                self.visit_Literal(
                    ops.Literal(row[column], dtype), value=row[column], dtype=dtype
                )
                for column, dtype in schema.items()
            )
            for row in rows
        ]
        if values:
            return sg.select(
                *(sg.column(column, quoted=True) for column in schema.names)
            ).from_(
                sge.values(
                    values,
                    alias="_ckdata_values",
                    columns=[
                        sg.to_identifier(column, quoted=True) for column in schema.names
                    ],
                )
            )
        return (
            sg.select(
                *(
                    self.cast(sge.Null(), dtype).as_(column, quoted=True)
                    for column, dtype in schema.items()
                )
            )
            .where(sge.false())
            .subquery(name)
        )


def _read_only(*args, **kwargs):
    raise NotImplementedError(
        "ckdata reads Athena; use an inline ibis.memtable() for a small lookup without creating a remote table"
    )


class Backend(SQLBackend):
    """Ibis Athena compiler connected to ckdata's read-only Rust transport."""

    name = "ckdata"
    compiler = _Compiler()
    create_table = create_view = drop_table = drop_view = insert = _read_only
    create_database = drop_database = truncate_table = rename_table = _read_only

    def __init__(self, connection):
        super().__init__(connection)
        self.connection = connection
        self._freshness = {}

    @property
    def version(self):
        from . import __version__

        return __version__

    @property
    def db_identity(self):
        # A lazy query must retain its exact connection across disconnection.
        return f"ckdata:{id(self.connection)}"

    @property
    def current_database(self):
        return self.connection.info["schema"]

    @property
    def current_catalog(self):
        return "AwsDataCatalog"

    def disconnect(self):
        self.connection.close()

    def list_tables(self, like=None, database=None):
        names = self.connection.list_tables(schema=database)
        return (
            names if like is None else [name for name in names if re.search(like, name)]
        )

    def get_schema(self, table_name, *, catalog=None, database=None):
        if catalog not in (None, "AwsDataCatalog"):
            raise ValueError("the shared reader currently uses AwsDataCatalog")
        columns = self.connection._columns(table_name, database)
        return sch.Schema(
            {
                name: self.compiler.type_mapper.from_string(dtype)
                for name, dtype in columns
            }
        )

    def table(self, name, /, *, database=None, freshness=None):
        check_freshness(freshness)
        self.connection._check_open()
        database = self.current_database if database is None else database
        result = super().table(name, database=database)
        if freshness is not None:
            # A public Ibis view gives this source call its own identity. Two
            # uses of the same table can retain different freshness policies.
            result = result.view()
            key = id(result.op())
            self._freshness[key] = (
                weakref.ref(result.op(), lambda ref: self._freshness.pop(key, None)),
                freshness,
            )
        return result

    def query_freshness(self, query):
        return [
            entry[1]
            for node in query.op().find(ops.SelfReference)
            if (entry := self._freshness.get(id(node))) is not None
            and entry[0]() is node
        ]

    def _get_schema_using_query(self, query):
        return sch.Schema.from_pyarrow(
            self.connection._query(query, schema_only=True).schema
        )

    def _register_in_memory_table(self, op):
        # _Compiler emits the data inline instead of registering a table.
        pass

    def prepare(self, expr, *, params=None, limit=None):
        from ._columns import apply_column_policy

        self.connection._check_open()
        expr = apply_column_policy(expr.as_table(), self, params=params)
        return expr, self.compile(expr, params=params, limit=limit)

    def to_pyarrow(self, expr, /, *, params=None, limit=None, timeout=None, **kwargs):
        if kwargs:
            raise TypeError(f"unknown collection argument: {', '.join(kwargs)}")
        _, sql = self.prepare(expr, params=params, limit=limit)
        table = self.connection._query(sql, timeout=timeout)
        # Ibis's default result mappers cast to its inferred schema. Retain the
        # Parquet reader's types and apply only the requested result shape.
        if isinstance(expr, ibis.expr.types.Table):
            return table
        column = table.column(0)
        return column if isinstance(expr, ibis.expr.types.Column) else column[0]

    def to_pyarrow_batches(
        self, expr, /, *, params=None, limit=None, chunk_size=1_000_000, **kwargs
    ):
        if (
            isinstance(chunk_size, bool)
            or not isinstance(chunk_size, int)
            or chunk_size <= 0
        ):
            raise ValueError("chunk_size must be a positive integer")
        return self.to_pyarrow(
            expr.as_table(), params=params, limit=limit, **kwargs
        ).to_reader(max_chunksize=chunk_size)

    def execute(self, expr, /, *, params=None, limit=None, **kwargs):
        # Arrow-backed pandas keeps exact nullable int64, decimal and nested data.
        import pandas as pd

        table = self.to_pyarrow(expr.as_table(), params=params, limit=limit, **kwargs)
        frame = table.to_pandas(types_mapper=pd.ArrowDtype)
        if isinstance(expr, ibis.expr.types.Table):
            return frame
        column = frame.iloc[:, 0]
        return column if isinstance(expr, ibis.expr.types.Column) else column.iloc[0]


def query_backend(query):
    if not isinstance(query, ibis.expr.types.Table):
        raise TypeError("query must be a lazy Ibis table")
    return ibis.get_backend(query)


def collect(query, *, params=None, timeout=None) -> pa.Table:
    """Collect an Ibis query as Arrow; sort after retrieval when order matters."""
    backend = query_backend(query)
    kwargs = {} if timeout is None else {"timeout": timeout}
    return backend.to_pyarrow(query, params=params, **kwargs)


def show_query(query, *, params=None) -> None:
    """Print the generated SQL without executing the query."""
    print(query_backend(query).compile(query, params=params, pretty=True))
