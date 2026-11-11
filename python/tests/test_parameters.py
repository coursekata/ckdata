"""Values are encoded as native Athena parameter expressions, never SQL edits."""

from datetime import UTC, date, datetime, time, timedelta, timezone
from decimal import Decimal

import pytest
from ckdata._parameters import parameter_literal, parameter_literals


@pytest.mark.parametrize(
    "value,expected",
    [
        (None, "NULL"),
        (True, "TRUE"),
        (False, "FALSE"),
        (9007199254740993, "CAST(9007199254740993 AS BIGINT)"),
        (-(2**63), "CAST(-9223372036854775808 AS BIGINT)"),
        (1.25, "CAST(1.25 AS DOUBLE)"),
        (float("inf"), "CAST('Infinity' AS DOUBLE)"),
        (float("-inf"), "CAST('-Infinity' AS DOUBLE)"),
        (float("nan"), "CAST('NaN' AS DOUBLE)"),
        (
            Decimal("12345678901234567890.123456789"),
            "DECIMAL '12345678901234567890.123456789'",
        ),
        ("O'Brien; --", "'O''Brien; --'"),
        ("", "''"),
        (b"\x00\x01\xff", "X'0001ff'"),
        (date(2026, 1, 2), "DATE '2026-01-02'"),
        (
            datetime(2026, 1, 2, 3, 4, 5, 678901),
            "TIMESTAMP '2026-01-02 03:04:05.678901'",
        ),
        (
            datetime(2026, 1, 2, 3, tzinfo=timezone(timedelta(hours=-8))),
            "TIMESTAMP '2026-01-02 11:00:00.000000'",
        ),
        (time(3, 4, 5, 678901), "TIME '03:04:05.678901'"),
    ],
)
def test_scalar_parameter_encoding(value, expected):
    assert parameter_literal(value) == expected


@pytest.mark.parametrize(
    "value",
    [
        2**63,
        -(2**63) - 1,
        Decimal("1e39"),
        Decimal("NaN"),
        "a\0b",
        time(1, tzinfo=UTC),
        {"key": 1},
    ],
)
def test_unrepresentable_parameters_fail_before_submission(value):
    with pytest.raises((TypeError, ValueError)):
        parameter_literals([value])


def test_parameters_require_positional_sequence():
    with pytest.raises(TypeError):
        parameter_literals({"name": "value"})
