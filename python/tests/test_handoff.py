"""An offline Rust Arrow fixture reaches the supported Python frame libraries."""

from datetime import datetime

import ckdata
import duckdb
import pandas as pd
import polars as pl
import pyarrow as pa


def test_arrow_handoff_preserves_values_across_frame_libraries():
    expected_rows = [
        {
            "id": 1,
            "s_null": None,
            "s_empty": "",
            "big": 9007199254740993,
            "ts": datetime(2026, 10, 16, 12, 34, 56, 789000),
        },
        {"id": 2, "s_null": "a", "s_empty": "b", "big": None, "ts": None},
    ]
    expected_types = {
        "id": pa.int32(),
        "s_null": pa.string(),
        "s_empty": pa.string(),
        "big": pa.int64(),
        "ts": pa.timestamp("ms"),
    }
    table = pa.table(ckdata._arrow_fixture()).sort_by("id")
    assert table.to_pylist() == expected_rows
    assert {field.name: field.type for field in table.schema} == expected_types

    frame = (
        pa.table(ckdata._arrow_fixture())
        .to_pandas(types_mapper=pd.ArrowDtype)
        .sort_values("id")
    )
    expected_frame = pd.DataFrame(
        {
            name: pd.array(
                [row[name] for row in expected_rows], dtype=pd.ArrowDtype(dtype)
            )
            for name, dtype in expected_types.items()
        }
    )
    pd.testing.assert_frame_equal(frame, expected_frame)

    r = ckdata._arrow_fixture()  # noqa: F841 - DuckDB's replacement scan reads it by name
    assert duckdb.sql(
        "select id, s_null, s_empty, big, ts from r order by id"
    ).fetchall() == [tuple(row.values()) for row in expected_rows]

    df = pl.DataFrame(ckdata._arrow_fixture()).sort("id")
    assert df.to_dicts() == expected_rows
    assert df.schema["ts"] == pl.Datetime("ms")


def test_version_matches_core():
    assert ckdata.__version__ == ckdata._core_versions()[0].removeprefix("ckdata ")
