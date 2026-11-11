"""Encode values for Athena's native ExecutionParameters API."""

from __future__ import annotations

import math
from datetime import UTC, date, datetime, time
from decimal import Decimal


def parameter_literal(value) -> str:
    if value is None:
        return "NULL"
    if isinstance(value, bool):
        return "TRUE" if value else "FALSE"
    if isinstance(value, int):
        if not -(2**63) <= value < 2**63:
            raise ValueError(
                "integer parameters must fit in a signed 64-bit integer; use Decimal for wider values"
            )
        return f"CAST({value} AS BIGINT)"
    if isinstance(value, float):
        special = (
            "NaN" if math.isnan(value) else "Infinity" if value > 0 else "-Infinity"
        )
        text = repr(value) if math.isfinite(value) else f"'{special}'"
        return f"CAST({text} AS DOUBLE)"
    if isinstance(value, Decimal):
        if not value.is_finite():
            raise ValueError("decimal parameters must be finite")
        precision = max(len(value.as_tuple().digits), -value.as_tuple().exponent)
        precision += max(0, value.as_tuple().exponent)
        if precision > 38:
            raise ValueError(
                "decimal parameters must fit within Athena's 38-digit precision"
            )
        return f"DECIMAL '{value:f}'"
    if isinstance(value, str):
        if "\0" in value:
            raise ValueError("string parameters cannot contain NUL characters")
        return "'" + value.replace("'", "''") + "'"
    if isinstance(value, bytes):
        return f"X'{value.hex()}'"
    if isinstance(value, datetime):
        if value.tzinfo is not None:
            value = value.astimezone(UTC).replace(tzinfo=None)
        return f"TIMESTAMP '{value.isoformat(sep=' ', timespec='microseconds')}'"
    if isinstance(value, date):
        return f"DATE '{value.isoformat()}'"
    if isinstance(value, time):
        if value.tzinfo is not None:
            raise ValueError("time parameters must not contain a timezone")
        return f"TIME '{value.isoformat(timespec='microseconds')}'"
    raise TypeError(f"unsupported parameter type: {type(value).__name__}")


def parameter_literals(params) -> list[str]:
    if params is None:
        return []
    if not isinstance(params, (list, tuple)):
        raise TypeError("params must be a positional list or tuple of scalar values")
    return [parameter_literal(value) for value in params]
