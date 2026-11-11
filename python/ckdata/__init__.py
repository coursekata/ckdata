"""Read Amazon Athena through Arrow, with optional lazy Ibis queries."""

from ._connection import (
    Connection,
    _ibis_module,
    connect,
    lake_disconnect,
    lake_query,
    lake_tbl,
)
from ._errors import (
    CkDataError,
    ColumnPolicyError,
    ColumnWarning,
    SavedResultWarning,
    SignInError,
    SnapshotError,
    StaleResultWarning,
)
from ._freshness import daily_at, freshness_check
from ._native import __version__
from ._native import _arrow_fixture as _arrow_fixture
from ._native import _core_versions as _core_versions
from ._saved import collect_saved
from ._settings import configure, options


def collect(query, *, params=None, timeout=None):
    """Retrieve a lazy Ibis query as a PyArrow Table with its original types."""
    return _ibis_module().collect(query, params=params, timeout=timeout)


def show_query(query, *, params=None) -> None:
    """Print generated SQL without executing a lazy Ibis query."""
    _ibis_module().show_query(query, params=params)


__all__ = [
    "CkDataError",
    "ColumnPolicyError",
    "ColumnWarning",
    "Connection",
    "SavedResultWarning",
    "SignInError",
    "SnapshotError",
    "StaleResultWarning",
    "__version__",
    "collect",
    "collect_saved",
    "configure",
    "connect",
    "daily_at",
    "freshness_check",
    "lake_disconnect",
    "lake_query",
    "lake_tbl",
    "options",
    "show_query",
]
