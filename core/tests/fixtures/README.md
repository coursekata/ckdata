# Reader fixtures

`temporal_int96.parquet` contains invented timestamps in years 1, 1600, 1969,
2300 and 9999, plus NULLs, lists, structs and maps. PyArrow 26 wrote its physical
INT96 columns without an embedded Arrow schema, with Snappy compression and two
rows per row group. This exercises the same timestamp representation as Athena
UNLOAD, including dates outside the nanosecond range.

Regenerate from the repository root using the existing development environment:

```sh
uv run --locked python - <<'PY'
from datetime import datetime
import pyarrow as pa
import pyarrow.parquet as pq

values = [
    datetime(1600, 1, 2, 3, 4, 5, 678000),
    datetime(1969, 12, 31, 23, 59, 59, 999000),
    datetime(2300, 2, 3, 4, 5, 6, 789000),
    datetime(9999, 12, 31, 23, 59, 59, 999000),
    None,
    datetime(1, 1, 1, 0, 0, 0, 1000),
]
timestamp = pa.timestamp("us")
table = pa.table({
    "stamp": pa.array(values, type=timestamp),
    "stamps": pa.array([[v, None] if v else [] for v in values], type=pa.list_(timestamp)),
    "record": pa.array([{"stamp": v} if v else None for v in values], type=pa.struct([("stamp", timestamp)])),
    "mapping": pa.array([[('a', v), ('b', None)] if v else None for v in values], type=pa.map_(pa.string(), timestamp)),
})
path = "core/tests/fixtures/temporal_int96.parquet"
pq.write_table(table, path, compression="snappy", store_schema=False,
               use_deprecated_int96_timestamps=True, row_group_size=2)
assert pq.read_table(path, coerce_int96_timestamp_unit="us").to_pylist() == table.to_pylist()
PY
```
