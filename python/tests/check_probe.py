"""Fails unless the core's literal probe reaches Polars, pyarrow, pandas and DuckDB unchanged."""
import duckdb
import pandas as pd
import polars as pl
import pyarrow as pa

import ckspike

print(ckspike.versions())
assert isinstance(pa.table(ckspike.probe()), pa.Table)
assert isinstance(pd.DataFrame.from_arrow(ckspike.probe()), pd.DataFrame)
r = ckspike.probe()
assert duckdb.sql("select count(*) from r").fetchall() == [(2,)]
df = pl.DataFrame(ckspike.probe()).sort("id")
assert df["s_null"][0] is None, "a NULL string must stay NULL"
assert df["s_empty"][0] == "", "an empty string must stay empty"
assert df["big"][0] == 9007199254740993, "a 64-bit integer past 2^53 must stay exact"
assert str(df.schema["ts"]) == "Datetime(time_unit='ms', time_zone=None)", df.schema["ts"]
print("handoff ok:", dict(df.schema))
