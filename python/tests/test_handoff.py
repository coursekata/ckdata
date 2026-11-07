"""An offline Rust Arrow fixture reaches the supported Python frame libraries."""

import ckdata
import duckdb
import pandas as pd
import polars as pl
import pyarrow as pa


def test_handoff_reaches_every_frame_library():
    assert isinstance(pa.table(ckdata._arrow_fixture()), pa.Table)
    assert isinstance(pd.DataFrame.from_arrow(ckdata._arrow_fixture()), pd.DataFrame)
    r = ckdata._arrow_fixture()  # noqa: F841 - DuckDB's replacement scan reads it by name
    assert duckdb.sql("select count(*) from r").fetchall() == [(2,)]
    df = pl.DataFrame(ckdata._arrow_fixture()).sort("id")
    assert df["s_null"][0] is None, "a NULL string must stay NULL"
    assert df["s_empty"][0] == "", "an empty string must stay empty"
    assert df["big"][0] == 9007199254740993, (
        "a 64-bit integer past 2^53 must stay exact"
    )
    assert str(df.schema["ts"]) == "Datetime(time_unit='ms', time_zone=None)", (
        df.schema["ts"]
    )


def test_version_matches_core():
    assert ckdata.__version__ == ckdata._core_versions()[0].removeprefix("ckdata ")
