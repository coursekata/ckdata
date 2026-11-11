"""Freshness policies for saved results, evaluated only during collection."""

from __future__ import annotations

import math
import re
import time
from collections.abc import Callable
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from typing import Any
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

from ._settings import current_options, env_setting, string


@dataclass(frozen=True)
class Daily:
    at: str
    tz: str


@dataclass(eq=False)
class RevisionCheck:
    check: Callable
    every: float
    _revisions: dict[str, tuple[float, str]] = field(default_factory=dict, repr=False)

    def revision(self, connection, context: str) -> str:
        cached = self._revisions.get(context)
        if cached is not None and time.monotonic() - cached[0] < self.every:
            return cached[1]
        value = string(self.check(connection))
        if self.every > 0:
            self._revisions[context] = (time.monotonic(), value)
        return value


def daily_at(at: str, tz: str) -> Daily:
    """Reuse saved results until a daily HH:MM refresh in an IANA timezone.

    The schedule assumes the source has rebuilt. A skipped clock time uses
    the transition boundary; a repeated clock time uses its first occurrence.
    """
    if not re.fullmatch(r"(?:[01][0-9]|2[0-3]):[0-5][0-9]", string(at)):
        raise ValueError("at must be a time in HH:MM format")
    try:
        ZoneInfo(string(tz))
    except (ZoneInfoNotFoundError, ValueError) as error:
        raise ValueError("tz must be a recognized IANA timezone") from error
    return Daily(at, tz)


def freshness_check(check: Callable, every: float = 300) -> RevisionCheck:
    """Check a completed-build revision, reusing it per source for every seconds.

    check(connection) must return one nonempty revision string without changing
    source data. Use every=0 to check on each collection.
    """
    if not callable(check):
        raise TypeError("check must be a function accepting a connection")
    if (
        isinstance(every, bool)
        or not isinstance(every, (int, float))
        or math.isnan(every)
        or every < 0
    ):
        raise ValueError("every must be a nonnegative number of seconds")
    return RevisionCheck(check, float(every))


def check_freshness(policy: Any) -> None:
    if (
        policy is None
        or isinstance(policy, (Daily, RevisionCheck))
        or callable(policy)
        or isinstance(policy, str)
        and policy == "always"
    ):
        return
    raise TypeError(
        "freshness must be None, 'always', daily_at(), freshness_check(), or a revision-checking function"
    )


def default_freshness():
    policy = current_options().get("freshness")
    if policy is not None:
        return policy
    at, tz = env_setting("FRESHNESS_AT"), env_setting("FRESHNESS_TZ")
    if at is None and tz is None:
        return "always"
    if at is None or tz is None:
        raise ValueError("set both CKDATA_FRESHNESS_AT and CKDATA_FRESHNESS_TZ")
    return daily_at(at, tz)


def resolve_freshness(query, backend, override):
    check_freshness(override)
    if override is not None:
        return override
    policies = (
        backend.query_freshness(query) if hasattr(backend, "query_freshness") else []
    )
    if policies:
        if any(policy != policies[0] for policy in policies[1:]):
            raise ValueError(
                "joined tables have different freshness policies; supply freshness to collect_saved()"
            )
        return policies[0]
    connection = getattr(backend, "connection", None)
    return getattr(connection, "freshness", None) or default_freshness()


def _local_boundary(local: datetime, zone: ZoneInfo) -> float:
    candidates = [local.replace(tzinfo=zone, fold=fold).timestamp() for fold in (0, 1)]
    valid = [
        value
        for value in candidates
        if datetime.fromtimestamp(value, zone).replace(tzinfo=None) == local
    ]
    if valid:
        return min(valid)
    # A gap has a valid time on each side. Find its transition using the
    # standard timezone database, rather than shifting the requested wall time.
    lower, upper = sorted(candidates)
    while upper - lower > 1:
        middle = math.floor((lower + upper) / 2)
        if datetime.fromtimestamp(middle, zone).replace(tzinfo=None) < local:
            lower = middle
        else:
            upper = middle
    return upper


def daily_boundary(policy: Daily, now: float) -> float:
    zone = ZoneInfo(policy.tz)
    day = datetime.fromtimestamp(now, zone).date()
    hour, minute = map(int, policy.at.split(":"))
    boundaries = [
        _local_boundary(
            datetime.combine(day - timedelta(days=offset), datetime.min.time()).replace(
                hour=hour, minute=minute
            ),
            zone,
        )
        for offset in (0, 1)
    ]
    return max(value for value in boundaries if value <= now)


def source_revision(policy, connection, context: str) -> str | None:
    if isinstance(policy, RevisionCheck):
        return policy.revision(connection, context)
    return string(policy(connection)) if callable(policy) else None


def is_fresh(saved, policy, revision: str | None, now: float) -> bool:
    if saved is None:
        return False
    if isinstance(policy, Daily):
        return daily_boundary(policy, now) <= saved.info["fetched_at"] <= now
    if isinstance(policy, RevisionCheck) or callable(policy):
        return revision is not None and saved.info.get("revision") == revision
    return False
