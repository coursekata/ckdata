"""Opt-in acceptance uses invented literals and only the synthetic types table."""

import json
import os
from datetime import datetime
from decimal import Decimal
from pathlib import Path

import ckdata
import ibis
import pyarrow as pa
import pyarrow.parquet as pq
import pytest

from .acceptance import validate_deployment, validate_identity, validate_workgroup

pytestmark = pytest.mark.live


@pytest.fixture(scope="module")
def live_connection():
    if os.environ.get("CKDATA_RUN_LIVE_TESTS") != "true":
        pytest.skip("synthetic AWS acceptance requires CKDATA_RUN_LIVE_TESTS=true")
    path, seat_name, profile = (
        os.environ.get(name)
        for name in (
            "CKDATA_TEST_DEPLOYMENT",
            "CKDATA_TEST_SEAT",
            "CKDATA_TEST_PROFILE",
        )
    )
    if not path or not seat_name or not profile:
        pytest.fail(
            "set CKDATA_TEST_DEPLOYMENT, CKDATA_TEST_SEAT, and CKDATA_TEST_PROFILE explicitly"
        )
    deployment = json.loads(Path(path).read_text())
    seat = validate_deployment(deployment, seat_name)
    with (
        ckdata.options(login=False, mode="explore", timeout=120),
        ckdata.connect(
            profile=profile,
            region=deployment["region"],
            role_arn=seat["role_arn"],
            workgroup=seat["workgroup"],
            schema=deployment["database"],
            output_location=None,
            login=False,
        ) as connection,
    ):
        validate_identity(connection.info, deployment, seat_name)
        validate_workgroup(
            ckdata._auth.wait(connection._handle.workgroup()), deployment, seat
        )
        yield connection


def test_synthetic_metadata_lazy_join_and_inline_lookup(live_connection):
    con = live_connection
    assert con.list_tables() == ["types"]
    table = con.table("types")
    assert set(table.columns) == {"id", "s_null", "s_empty", "big", "dec", "ts", "arr"}
    selected = table.select("id", "s_empty", "big")
    ckdata.show_query(selected)
    rows = ckdata.collect(selected).sort_by("id")
    assert rows.column("big").to_pylist() == [9007199254740993, None]
    lookup = ibis.memtable(
        pa.table(
            {"id": pa.array([1, 2], type=pa.int32()), "label": ["first", "second"]}
        )
    )
    joined = selected.left_join(lookup, "id").select("id", "label")
    assert ckdata.collect(joined).sort_by("id").to_pylist() == [
        {"id": 1, "label": "first"},
        {"id": 2, "label": "second"},
    ]
    counted = table.group_by(has_big=table.big.notnull()).aggregate(n=ibis._.count())
    assert ckdata.collect(counted).sort_by("has_big").column("n").to_pylist() == [1, 1]


def test_synthetic_scalar_nested_and_empty_schemas(live_connection):
    sql = """SELECT
      BIGINT '9007199254740993' AS big,
      CAST('12345678901234567890.123456789012345678' AS DECIMAL(38,18)) AS wide,
      CAST(NULL AS VARCHAR) AS missing, '' AS empty,
      TIMESTAMP '1600-01-02 03:04:05.678' AS stamp,
      ARRAY[1, CAST(NULL AS INTEGER)] AS items,
      CAST(ROW(BIGINT '3', ARRAY[1, CAST(NULL AS INTEGER)]) AS ROW(id BIGINT, items ARRAY(INTEGER))) AS record_value,
      MAP(ARRAY['a','b'], ARRAY[DECIMAL '1.10', CAST(NULL AS DECIMAL(3,2))]) AS mapping"""
    result = ckdata.lake_query(sql, con=live_connection)
    assert result.to_pylist() == [
        {
            "big": 9007199254740993,
            "wide": Decimal("12345678901234567890.123456789012345678"),
            "missing": None,
            "empty": "",
            "stamp": datetime(1600, 1, 2, 3, 4, 5, 678000),
            "items": [1, None],
            "record_value": {"id": 3, "items": [1, None]},
            "mapping": [("a", Decimal("1.10")), ("b", None)],
        }
    ]
    for empty_sql in (f"SELECT * FROM ({sql}) invented WHERE FALSE", f"{sql} LIMIT 0"):
        empty = ckdata.lake_query(empty_sql, con=live_connection)
        assert empty.num_rows == 0
        assert empty.schema.equals(result.schema, check_metadata=True)


def test_synthetic_native_parameters_and_typed_missing(live_connection):
    result = ckdata.lake_query(
        "SELECT ? AS text_value, ? AS big, ? AS wide, ? AS bytes",
        con=live_connection,
        params=[
            "O'Brien",
            9007199254740993,
            Decimal("12345678901234567890.123456789"),
            bytes([0, 1, 255]),
        ],
    )
    assert result.to_pylist() == [
        {
            "text_value": "O'Brien",
            "big": 9007199254740993,
            "wide": Decimal("12345678901234567890.123456789"),
            "bytes": bytes([0, 1, 255]),
        }
    ]
    table = live_connection.table("types")
    parameter = ibis.param("int32")
    query = table.filter(table.id == parameter).select(
        "id", missing=ibis.literal(None, type="int64")
    )
    result = ckdata.collect(query, params={parameter: 1})
    assert result.to_pylist() == [{"id": 1, "missing": None}]
    assert result.schema.field("missing").type == pa.int64()


def test_synthetic_saved_reuse_refresh_and_parquet_snapshot(live_connection, tmp_path):
    query = live_connection.table("types")
    policy = ckdata.daily_at("00:00", "UTC")
    first = ckdata.collect_saved(query, freshness=policy, dir=tmp_path)
    query_id = live_connection.last_query_id
    assert ckdata.collect_saved(query, freshness=policy, dir=tmp_path).equals(first)
    assert live_connection.last_query_id == query_id
    assert (
        ckdata.collect_saved(query, refresh=True, dir=tmp_path)
        .sort_by("id")
        .equals(first.sort_by("id"))
    )
    assert live_connection.last_query_id != query_id
    snapshot = ckdata.collect_saved(query, snapshot="baseline", dir=tmp_path)
    query_id = live_connection.last_query_id
    assert ckdata.collect_saved(query, snapshot="baseline", dir=tmp_path).equals(
        snapshot
    )
    assert live_connection.last_query_id == query_id
    saved = pq.ParquetFile(next(tmp_path.glob("snapshot-baseline-*.parquet"))).read()
    assert saved.column("big").type == pa.int64()
    assert saved.sort_by("id").column("big").to_pylist() == [9007199254740993, None]
