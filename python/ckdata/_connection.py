"""Explicit connections and the researcher-facing managed session."""

from __future__ import annotations

import importlib
from threading import RLock

import pyarrow as pa

from . import _native
from ._auth import wait
from ._parameters import parameter_literals
from ._settings import (
    connection_settings,
    current_options,
    env_setting,
    seconds,
    string,
)


def _ibis_module():
    try:
        return importlib.import_module("._ibis", __package__)
    except ModuleNotFoundError as error:
        if error.name == "ibis":
            raise ImportError(
                "Lazy queries require the optional Ibis integration: install ckdata[ibis]."
            ) from error
        raise


class Connection:
    """A read-only Athena connection with AWS-managed renewable credentials.

    Use connect() or a with block to create one. Existing lazy queries keep
    this connection after it is closed; rebuild them to use a new session.
    """

    def __init__(self, handle, *, freshness=None):
        self._handle = handle
        self.freshness = freshness
        self._backend = None
        self.last_query_id: str | None = None

    @property
    def closed(self) -> bool:
        return self._handle.closed

    @property
    def info(self) -> dict[str, str]:
        """Selected account, ARN, region, database and workgroup; no credentials."""
        self._check_open()
        return self._handle.info()

    def _check_open(self) -> None:
        if self.closed:
            raise _native.Error("ckdata: connection is closed")

    def close(self) -> None:
        """Close the connection without authenticating or opening another one."""
        self._handle.close()

    def __enter__(self):
        self._check_open()
        return self

    def __exit__(self, *exc):
        self.close()

    def _query(
        self, sql: str, *, params=None, timeout=None, schema_only=False
    ) -> pa.Table:
        self._check_open()
        if not isinstance(sql, str) or not sql.strip() or "\0" in sql:
            raise ValueError("sql must be one nonempty string without NUL characters")
        timeout = seconds(
            current_options().get("timeout", 1800) if timeout is None else timeout
        )
        result = wait(
            self._handle.query(sql, timeout, parameter_literals(params), schema_only)
        )
        self.last_query_id = result["query_id"]
        return pa.table(result["reader"])

    def _columns(self, table: str, schema: str | None = None) -> list[tuple[str, str]]:
        self._check_open()
        schema = self.info["schema"] if schema is None else string(schema)
        return wait(self._handle.metadata(schema, string(table)))

    def list_tables(self, *, schema: str | None = None) -> list[str]:
        """Discover table names using catalog metadata without reading rows."""
        self._check_open()
        schema = self.info["schema"] if schema is None else string(schema)
        return wait(self._handle.metadata(schema))

    def list_fields(self, table: str, *, schema: str | None = None) -> list[str]:
        """Discover a table's column names without reading rows."""
        return [name for name, _ in self._columns(table, schema)]

    def _ibis(self):
        self._check_open()
        if self._backend is None:
            self._backend = _ibis_module().Backend(self)
        return self._backend

    def table(self, table: str, *, schema: str | None = None, freshness=None):
        """Open an Ibis table; optionally set source-specific saved freshness."""
        return self._ibis().table(table, database=schema, freshness=freshness)

    def sql(self, sql: str, *, schema=None):
        """Build a lazy Ibis query; schema inference uses a schema-only query."""
        return self._ibis().sql(sql, schema=schema)


def connect(**settings) -> Connection:
    """Open an explicit connection without submitting a data query.

    Settings include workgroup, schema, profile, region, role_arn,
    output_location, login, freshness, and individual sso_* fields. Arguments
    override configure()/options(), then CKDATA_* environment settings. A
    selected profile is authoritative. login=False prevents browser sign-in.
    """
    resolved = connection_settings(settings)
    handle = wait(_native.connect(resolved.values, resolved.sso, resolved.login))
    return Connection(handle, freshness=resolved.freshness)


_managed: Connection | None = None
_managed_lock = RLock()


def _lake_con() -> Connection:
    global _managed
    with _managed_lock:
        if _managed is None or _managed.closed:
            _managed = connect()
        return _managed


def lake_tbl(table: str, schema: str | None = None, freshness=None):
    """Open a lazy Ibis table on the managed session without fetching rows."""
    _ibis_module()
    if schema is None:
        schema = current_options().get("schema", env_setting("SCHEMA")) or "default"
    return _lake_con().table(table, schema=schema, freshness=freshness)


def lake_query(
    sql: str, *, con: Connection | None = None, params=None, timeout=None
) -> pa.Table:
    """Run SQL directly and return its original Arrow types.

    params is a positional list/tuple of scalar values, passed through Athena's
    native parameter API. Direct SQL bypasses lazy-table column heuristics.
    UNLOAD does not guarantee global ordering; sort the downloaded result.
    """
    return (con if con is not None else _lake_con())._query(
        sql, params=params, timeout=timeout
    )


def lake_disconnect() -> None:
    """Close the managed session; the next use reads the current settings."""
    global _managed
    with _managed_lock:
        if _managed is not None:
            _managed.close()
            _managed = None
