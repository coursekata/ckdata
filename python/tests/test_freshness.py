"""Freshness precedence, completed revisions and daylight-saving boundaries."""

from datetime import UTC, datetime
from types import SimpleNamespace

import ckdata
import pytest
from ckdata import _freshness


def stamp(*args):
    return datetime(*args, tzinfo=UTC).timestamp()


@pytest.mark.parametrize(
    "policy,now,expected",
    [
        (
            ckdata.daily_at("07:00", "America/Los_Angeles"),
            stamp(2026, 1, 2, 14),
            stamp(2026, 1, 1, 15),
        ),
        (
            ckdata.daily_at("02:30", "America/Los_Angeles"),
            stamp(2026, 3, 8, 11),
            stamp(2026, 3, 8, 10),
        ),
        (
            ckdata.daily_at("01:30", "America/Los_Angeles"),
            stamp(2026, 11, 1, 9, 15),
            stamp(2026, 11, 1, 8, 30),
        ),
        (ckdata.daily_at("00:00", "UTC"), stamp(2028, 2, 29, 1), stamp(2028, 2, 29)),
    ],
)
def test_daily_refresh_uses_correct_boundary_including_skipped_and_repeated_times(
    policy, now, expected
):
    assert _freshness.daily_boundary(policy, now) == expected


def test_revision_checks_are_reused_per_source_and_not_cached_on_failure(monkeypatch):
    revisions = iter(["build-a", RuntimeError("unavailable"), "build-b"])
    calls = []

    def check(con):
        calls.append(con)
        value = next(revisions)
        if isinstance(value, Exception):
            raise value
        return value

    monkeypatch.setattr(_freshness.time, "monotonic", lambda: 0)
    policy = ckdata.freshness_check(check, every=10)
    assert policy.revision("connection", "source-a") == "build-a"
    assert policy.revision("connection", "source-a") == "build-a"
    with pytest.raises(RuntimeError):
        policy.revision("connection", "source-b")
    assert policy.revision("connection", "source-b") == "build-b"
    assert len(calls) == 3


def test_collection_then_table_then_connection_then_application_then_environment(
    monkeypatch,
):
    monkeypatch.setenv("CKDATA_FRESHNESS_AT", "07:00")
    monkeypatch.setenv("CKDATA_FRESHNESS_TZ", "UTC")
    backend = SimpleNamespace(
        connection=SimpleNamespace(freshness=None), query_freshness=lambda query: []
    )
    resolve = lambda override=None: _freshness.resolve_freshness(
        None, backend, override
    )
    assert resolve() == ckdata.daily_at("07:00", "UTC")
    ckdata.configure(freshness=ckdata.daily_at("08:00", "UTC"))
    assert resolve() == ckdata.daily_at("08:00", "UTC")
    backend.connection.freshness = "always"
    assert resolve() == "always"
    backend.query_freshness = lambda query: [ckdata.daily_at("09:00", "UTC")]
    assert resolve() == ckdata.daily_at("09:00", "UTC")
    assert resolve("always") == "always"


def test_joined_source_conflict_requires_collection_override(connection):
    first = connection.table("types", freshness=ckdata.daily_at("07:00", "UTC"))
    second = connection.table("types", freshness=ckdata.daily_at("08:00", "UTC"))
    joined = first.join(second, first.id == second.id)
    backend = connection._ibis()
    with pytest.raises(ValueError, match="different freshness"):
        _freshness.resolve_freshness(joined, backend, None)
    assert _freshness.resolve_freshness(joined, backend, "always") == "always"
    connection._handle.query.assert_not_called()


@pytest.mark.parametrize(
    "at,tz", [("7:00", "UTC"), ("24:00", "UTC"), ("07:00", "Not/AZone")]
)
def test_invalid_daily_schedule_is_rejected(at, tz):
    with pytest.raises(ValueError):
        ckdata.daily_at(at, tz)
