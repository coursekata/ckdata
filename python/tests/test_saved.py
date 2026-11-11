"""Saved-result identity, failures and filesystem publication using invented data."""

from concurrent.futures import ThreadPoolExecutor
from datetime import UTC, datetime
from pathlib import Path
from threading import Barrier
from unittest.mock import Mock, patch

import ckdata
import ibis
import pyarrow as pa
import pyarrow.parquet as pq
import pytest
from ckdata import _freshness, _saved


def test_daily_reuse_refresh_and_snapshots_preserve_arrow_values(connection, tmp_path):
    query = connection.table("types")
    policy = ckdata.daily_at("00:00", "UTC")
    first = ckdata.collect_saved(query, freshness=policy, dir=tmp_path)
    assert ckdata.collect_saved(query, freshness=policy, dir=tmp_path).equals(
        first, check_metadata=True
    )
    assert connection._handle.query.call_count == 1
    assert ckdata.collect_saved(
        query, freshness=policy, refresh=True, dir=tmp_path
    ).equals(first)
    assert connection._handle.query.call_count == 2
    frozen = ckdata.collect_saved(query, snapshot="baseline", dir=tmp_path)
    assert ckdata.collect_saved(query, snapshot="baseline", dir=tmp_path).equals(frozen)
    assert connection._handle.query.call_count == 3
    table = pq.ParquetFile(next(tmp_path.glob("snapshot-baseline-*.parquet"))).read()
    assert table.column("big").type == pa.int64()
    assert table.column("big").to_pylist() == [9007199254740993, None]


@pytest.mark.parametrize("container", [set, frozenset], ids=["set", "frozenset"])
def test_unordered_allowlists_reuse_the_same_snapshot_as_ordered_names(
    connection, query_result, tmp_path, container
):
    query = connection.table("types")
    names = ["item_id", "lrn_type"]
    with ckdata.options(allow={"mart_responses": names}):
        original = ckdata.collect_saved(query, snapshot="baseline", dir=tmp_path)
    query_result["reader"] = original.slice(0, 1)
    with ckdata.options(allow={"mart_responses": container(names)}):
        reused = ckdata.collect_saved(query, snapshot="baseline", dir=tmp_path)
    assert reused.equals(original, check_metadata=True)
    connection._handle.query.assert_called_once()
    assert len(list(tmp_path.glob("snapshot-baseline-*.parquet"))) == 1


def test_crossing_refresh_boundary_fetches_again(connection, tmp_path, monkeypatch):
    clock = [datetime(2026, 11, 10, 14, tzinfo=UTC).timestamp()]
    monkeypatch.setattr(_saved.time, "time", lambda: clock[0])
    query = connection.table("types")
    policy = ckdata.daily_at("07:00", "America/Los_Angeles")
    ckdata.collect_saved(query, freshness=policy, dir=tmp_path)
    clock[0] += 3600
    ckdata.collect_saved(query, freshness=policy, dir=tmp_path)
    assert connection._handle.query.call_count == 2


def test_completed_build_revision_reuses_then_refreshes(connection, tmp_path):
    current = ["build-a"]
    policy = ckdata.freshness_check(lambda con: current[0], every=0)
    query = connection.table("types")
    ckdata.collect_saved(query, freshness=policy, dir=tmp_path)
    ckdata.collect_saved(query, freshness=policy, dir=tmp_path)
    assert connection._handle.query.call_count == 1
    current[0] = "build-b"
    ckdata.collect_saved(query, freshness=policy, dir=tmp_path)
    assert connection._handle.query.call_count == 2


def test_revision_cache_refreshes_changed_rows_at_expiry(
    connection, query_result, tmp_path, monkeypatch
):
    clock = [0.0]
    monkeypatch.setattr(_freshness.time, "monotonic", lambda: clock[0])
    revision = Mock(side_effect=["build-a", "build-b"])
    policy = ckdata.freshness_check(revision, every=10)
    query = connection.table("types")
    original = ckdata.collect_saved(query, freshness=policy, dir=tmp_path)
    changed = original.slice(0, 1)
    query_result["reader"] = changed

    clock[0] = 9.999
    assert ckdata.collect_saved(query, freshness=policy, dir=tmp_path).equals(original)
    revision.assert_called_once()
    connection._handle.query.assert_called_once()

    clock[0] = 10
    assert ckdata.collect_saved(query, freshness=policy, dir=tmp_path).equals(changed)
    assert revision.call_count == 2
    assert connection._handle.query.call_count == 2


def test_query_or_freshness_failure_returns_available_stale_data(connection, tmp_path):
    query = connection.table("types")
    first = ckdata.collect_saved(query, dir=tmp_path)
    connection._handle.query.side_effect = ckdata.CkDataError("synthetic query failure")
    with pytest.warns(ckdata.StaleResultWarning):
        assert ckdata.collect_saved(query, dir=tmp_path).equals(first)
    connection._handle.query.side_effect = None

    def unavailable(con):
        raise RuntimeError("synthetic revision failure")

    before = connection._handle.query.call_count
    with pytest.warns(ckdata.StaleResultWarning):
        assert ckdata.collect_saved(query, freshness=unavailable, dir=tmp_path).equals(
            first
        )
    assert connection._handle.query.call_count == before


def test_failure_without_saved_data_propagates_and_save_failure_returns_fresh(
    connection, tmp_path, monkeypatch
):
    query = connection.table("types")
    connection._handle.query.side_effect = ckdata.CkDataError("synthetic query failure")
    with pytest.raises(ckdata.CkDataError, match="synthetic query failure"):
        ckdata.collect_saved(query, dir=tmp_path)
    connection._handle.query.side_effect = None

    def no_space(*args):
        raise OSError("synthetic disk full")

    monkeypatch.setattr(_saved, "_write_saved", no_space)
    with pytest.warns(ckdata.SavedResultWarning, match="not saved"):
        assert ckdata.collect_saved(query, dir=tmp_path).num_rows == 2


def test_unreadable_normal_result_is_refetched_and_snapshot_is_preserved(
    connection, tmp_path
):
    query = connection.table("types")
    ckdata.collect_saved(query, dir=tmp_path)
    normal = next(tmp_path.glob("result-*.parquet"))
    normal.write_bytes(b"invented corrupt file")
    with pytest.warns(ckdata.SavedResultWarning, match="fetching again"):
        assert ckdata.collect_saved(query, dir=tmp_path).num_rows == 2
    ckdata.collect_saved(query, snapshot="baseline", dir=tmp_path)
    snapshot = next(tmp_path.glob("snapshot-*.parquet"))
    snapshot.write_bytes(b"invented corrupt snapshot")
    with pytest.raises(ckdata.SnapshotError):
        ckdata.collect_saved(query, snapshot="baseline", dir=tmp_path)
    assert snapshot.read_bytes() == b"invented corrupt snapshot"


@pytest.mark.parametrize("change", ["sql", "access", "mode", "allow"])
def test_saved_identity_changes_miss_then_reuse_independently(
    connection, tmp_path, change
):
    query = connection.table("types")
    policy = lambda con: "fixed-build"
    ckdata.collect_saved(query, freshness=policy, dir=tmp_path)
    settings = {}
    if change == "sql":
        query = query.select("id")
    elif change == "access":
        connection._handle.info.return_value["arn"] = (
            "arn:aws:sts::111111111111:assumed-role/other-synthetic-reader/test"
        )
    elif change == "mode":
        settings = {"mode": "audit"}
    else:
        settings = {"allow": {"types": ["id"]}}
    with ckdata.options(**settings):
        ckdata.collect_saved(query, freshness=policy, dir=tmp_path)
        assert connection._handle.query.call_count == 2
        ckdata.collect_saved(query, freshness=policy, dir=tmp_path)
        assert connection._handle.query.call_count == 2
    assert len(list(tmp_path.glob("result-*.parquet"))) == 2


def test_other_ibis_backends_require_explicit_source_context(tmp_path):
    backend = ibis.sqlite.connect(":memory:")
    try:
        query = backend.sql("SELECT 1 AS id")
        with pytest.raises(ValueError, match="supply source"):
            ckdata.collect_saved(query, dir=tmp_path)
        source = {"database": "invented", "principal": "invented-reader"}
        assert ckdata.collect_saved(
            query, snapshot=True, dir=tmp_path, source=source
        ).to_pylist() == [{"id": 1}]
        assert ckdata.collect_saved(
            query, snapshot=True, dir=tmp_path, source=source
        ).to_pylist() == [{"id": 1}]
    finally:
        backend.disconnect()


def test_parallel_snapshot_writers_return_the_first_complete_publication(tmp_path):
    path = tmp_path / "snapshot.parquet"
    barrier = Barrier(2)

    def publish(value):
        barrier.wait()
        return _saved._write_saved(
            pa.table({"id": [value]}), path, "invented", True, 1, None
        )

    with ThreadPoolExecutor(max_workers=2) as pool:
        results = list(pool.map(publish, [1, 2]))
    winner = pq.ParquetFile(path).read().column("id").to_pylist()
    assert winner in ([1], [2])
    assert all(result.column("id").to_pylist() == winner for result in results)
    assert not list(tmp_path.glob(".ckdata-*"))
    _saved._write_saved(pa.table({"id": [3]}), path, "invented", False, 2, None)
    assert pq.ParquetFile(path).read().column("id").to_pylist() == [3]


def test_normal_directory_is_temporary_and_snapshot_defaults_to_working_directory(
    tmp_path, monkeypatch
):
    monkeypatch.chdir(tmp_path)
    normal = _saved._saved_path(None, False, None, "invented")
    frozen = _saved._saved_path(None, True, "baseline", "invented")
    assert normal.parent.is_relative_to(Path(_saved._temporary.name))
    assert frozen.parent == tmp_path


def test_clearing_snapshot_directory_restores_working_directory(
    connection, tmp_path, monkeypatch
):
    monkeypatch.chdir(tmp_path)
    ckdata.configure(snapshot_dir=tmp_path / "previous")
    ckdata.configure(snapshot_dir=None)
    ckdata.collect_saved(connection.table("types"), snapshot="baseline")
    snapshots = list(tmp_path.glob("snapshot-baseline-*.parquet"))
    assert len(snapshots) == 1
    assert not (tmp_path / "previous").exists()


def test_parallel_first_use_shares_one_owned_temporary_directory(monkeypatch):
    monkeypatch.setattr(_saved, "_temporary", None)
    barrier = Barrier(8)

    def path(index):
        barrier.wait(timeout=5)
        return _saved._saved_path(None, False, None, str(index))

    try:
        with patch.object(
            _saved.tempfile,
            "TemporaryDirectory",
            wraps=_saved.tempfile.TemporaryDirectory,
        ) as create:
            with ThreadPoolExecutor(max_workers=8) as pool:
                paths = list(pool.map(path, range(8)))
            create.assert_called_once()
        assert len({path.parent for path in paths}) == 1
        assert all(path.parent.is_dir() for path in paths)
        directory = paths[0].parent
    finally:
        if _saved._temporary is not None:
            _saved._temporary.cleanup()
    assert not directory.exists()


@pytest.mark.parametrize(
    "kwargs",
    [
        {"snapshot": "../escape"},
        {"snapshot": 0},
        {"snapshot": True, "refresh": True},
        {"snapshot": "baseline", "freshness": "always"},
        {"refresh": 1},
    ],
)
def test_invalid_saved_result_arguments_fail_before_query(connection, tmp_path, kwargs):
    with pytest.raises((ValueError, TypeError)):
        ckdata.collect_saved(connection.table("types"), dir=tmp_path, **kwargs)
    assert not connection._handle.query.called
