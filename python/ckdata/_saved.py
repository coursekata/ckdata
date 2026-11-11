"""Parquet saved results with atomic publication and explicit freshness."""

from __future__ import annotations

import hashlib
import json
import math
import os
import re
import tempfile
import time
import warnings
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from threading import Lock

import pyarrow as pa
import pyarrow.parquet as pq

from ._errors import SavedResultWarning, SnapshotError, StaleResultWarning
from ._freshness import is_fresh, resolve_freshness, source_revision
from ._settings import boolean, current_options

_METADATA = b"ckdata.saved"
_temporary: tempfile.TemporaryDirectory | None = None
_temporary_lock = Lock()


@dataclass
class _Saved:
    table: pa.Table
    info: dict


def _json(value) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False)


def _source_context(backend, source):
    from ._ibis import Backend

    if source is not None and not isinstance(source, Mapping):
        raise TypeError(
            "source must be a mapping identifying the database/namespace and access context"
        )
    if isinstance(backend, Backend):
        context = backend.connection.info
    elif source is None:
        raise ValueError(
            "supply source with the database/namespace and access context for this Ibis backend"
        )
    else:
        context = None
    result = {
        "driver": f"{type(backend).__module__}.{type(backend).__qualname__}",
        "context": context,
        "source": None if source is None else dict(source),
    }
    _json(result)  # Reject unsupported or ambiguous identity values before any query.
    return result


def _snapshot_label(snapshot) -> str | None:
    if isinstance(snapshot, bool):
        return None
    if not isinstance(snapshot, str) or not re.fullmatch(
        r"[A-Za-z0-9][A-Za-z0-9._-]*", snapshot
    ):
        raise ValueError(
            "snapshot must be False, True, or a filename-safe label; use dir for the directory"
        )
    return snapshot


def _saved_path(directory, frozen: bool, label: str | None, key: str) -> Path:
    global _temporary
    if directory is None:
        settings = current_options()
        if frozen:
            directory = settings.get("snapshot_dir")
            if directory is None:
                directory = Path.cwd()
        else:
            directory = settings.get("saved_dir")
            if directory is None:
                with _temporary_lock:
                    if _temporary is None:
                        _temporary = tempfile.TemporaryDirectory(prefix="ckdata-saved-")
                    directory = _temporary.name
    prefix = "snapshot-" + (f"{label}-" if label else "") if frozen else "result-"
    return Path(directory) / f"{prefix}{key}.parquet"


def _read_saved(path: Path, key: str, frozen: bool) -> _Saved | None:
    if not path.exists():
        return None
    try:
        table = pq.ParquetFile(path).read()
        metadata = dict(table.schema.metadata or {})
        info = json.loads(metadata.pop(_METADATA))
        if not isinstance(info, dict):
            raise TypeError("saved-result metadata must be an object")
        stamp = info.get("fetched_at")
        if (
            info.get("key") != key
            or info.get("snapshot") is not frozen
            or isinstance(stamp, bool)
            or not isinstance(stamp, (int, float))
            or not math.isfinite(stamp)
        ):
            raise ValueError("saved-result identity or retrieval time does not match")
        return _Saved(table.replace_schema_metadata(metadata or None), info)
    except (OSError, ValueError, TypeError, KeyError, pa.ArrowException) as error:
        if not path.exists():
            return None
        if frozen:
            raise SnapshotError(
                f"Existing snapshot could not be read and was not replaced: {path}. Repair it or choose another label/directory."
            ) from error
        warnings.warn(
            f"Could not read saved result; fetching again: {error}",
            SavedResultWarning,
            stacklevel=3,
        )
        return None


def _warn_stale(saved: _Saved, path: Path, error: Exception) -> None:
    age = max(0, round(time.time() - saved.info["fetched_at"]))
    warnings.warn(
        f"Refresh failed; returning stale saved data ({age} seconds old) from {path}: {error}",
        StaleResultWarning,
        stacklevel=3,
    )


def _write_saved(table, path, key, frozen, fetched_at, revision):
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=".ckdata-", suffix=".parquet", dir=path.parent
    )
    os.close(descriptor)
    temporary = Path(temporary_name)
    try:
        metadata = dict(table.schema.metadata or {})
        metadata[_METADATA] = _json(
            {
                "key": key,
                "snapshot": frozen,
                "fetched_at": fetched_at,
                "revision": revision,
            }
        ).encode()
        pq.write_table(table.replace_schema_metadata(metadata), temporary)
        if not frozen:
            os.replace(temporary, path)
            return table
        try:
            os.link(temporary, path)
            return table
        except FileExistsError:
            winner = _read_saved(path, key, frozen=True)
            if winner is None:
                raise
            return winner.table
    finally:
        temporary.unlink(missing_ok=True)


def _revision(policy, connection, context, saved, path):
    try:
        return source_revision(policy, connection, _json(context)), False
    except Exception as error:  # noqa: BLE001 - user callbacks can raise any ordinary exception
        if saved is not None:
            _warn_stale(saved, path, error)
            return None, True
        warnings.warn(
            f"Could not check source freshness; fetching without a reusable revision marker: {error}",
            SavedResultWarning,
            stacklevel=3,
        )
        return None, False


def _fetch_and_save(fetch, saved, path, key, frozen, revision):
    fetched_at = time.time()
    try:
        table = fetch()
    except Exception as error:
        if saved is None:
            raise
        _warn_stale(saved, path, error)
        return saved.table
    try:
        return _write_saved(table, path, key, frozen, fetched_at, revision)
    except SnapshotError:
        raise
    except (OSError, ValueError, TypeError, pa.ArrowException) as error:
        warnings.warn(
            f"Fetched fresh data, but it was not saved to {path}: {error}",
            SavedResultWarning,
            stacklevel=3,
        )
        return table


def collect_saved(
    query,
    freshness=None,
    refresh=False,
    snapshot=False,
    dir=None,
    source=None,
    *,
    params=None,
) -> pa.Table:
    """Collect an Ibis query with temporary reuse or a durable Parquet snapshot.

    freshness overrides table, connection, application and environment policies.
    Without a policy, fetch anew. snapshot=True or a label saves indefinitely;
    snapshots default to the current directory and cannot specify freshness or
    refresh. Normal results default to a session temporary directory.

    Other Ibis backends require an explicit source mapping with database,
    namespace, principal/role, tenant and session settings affecting results.
    Never include credentials. Exact SQL, source context and column settings
    identify results; labels supplement that identity.

    Refresh failures return available stale data with a warning. Save failures
    return fresh data with a warning. Unreadable snapshots raise an error and
    are preserved. Files are ordinary Parquet, readable offline. Concurrent
    misses may duplicate queries; normal publication keeps the last complete
    file and snapshots retain and return the first complete publication.
    """
    from ._ibis import Backend, query_backend

    boolean(refresh)
    label = _snapshot_label(snapshot)
    frozen = snapshot is not False
    if frozen and (refresh or freshness is not None):
        raise ValueError(
            "a snapshot cannot specify refresh or freshness; choose another label/directory"
        )
    backend = query_backend(query)
    policy = None if frozen else resolve_freshness(query, backend, freshness)
    context = _source_context(backend, source)
    columns = None
    if isinstance(backend, Backend):
        query, sql = backend.prepare(query, params=params)
        settings = current_options()
        columns = {
            "mode": settings.get("mode", "explore"),
            "allow": settings.get("allow", {}),
        }
        fetch = lambda: backend.connection._query(sql)
    else:
        sql = backend.compile(query, params=params)
        fetch = lambda: backend.to_pyarrow(query, params=params)
    key = hashlib.sha256(
        _json(
            {"version": 2, "sql": sql, "source": context, "columns": columns}
        ).encode()
    ).hexdigest()
    path = _saved_path(dir, frozen, label, key)
    saved = _read_saved(path, key, frozen)
    if frozen and saved is not None:
        return saved.table
    revision, stale = (
        (None, False)
        if frozen
        else _revision(
            policy,
            getattr(backend, "connection", backend),
            context,
            None if refresh else saved,
            path,
        )
    )
    if stale:
        return saved.table
    if not refresh and not frozen and is_fresh(saved, policy, revision, time.time()):
        if current_options().get("verbose", False):
            print(f"ckdata: reading saved result from {path}")
        return saved.table
    return _fetch_and_save(fetch, saved, path, key, frozen, revision)
