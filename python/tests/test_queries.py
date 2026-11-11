"""Query composition, native-boundary forwarding and Arrow result shapes."""

from unittest.mock import Mock

import ckdata
import duckdb
import ibis
import pyarrow as pa
import pytest
from ckdata import _native


def test_discovery_and_sql_display_do_not_execute_and_collect_uses_displayed_sql(
    managed_connection, query_result, capsys
):
    assert managed_connection.list_tables() == ["types"]
    table = ckdata.lake_tbl("types")
    assert tuple(table.columns) == ("id", "s_null", "s_empty", "big", "ts")
    selected = table.filter(table.id == 1).select("id", "s_empty", "big")
    ckdata.show_query(selected)
    displayed = capsys.readouterr().out
    assert "WHERE" in displayed
    managed_connection._handle.query.assert_not_called()
    query_result["reader"] = (
        query_result["reader"].select(["id", "s_empty", "big"]).slice(0, 1)
    )
    actual = ckdata.collect(selected)
    assert actual.equals(query_result["reader"], check_metadata=True)
    assert actual.schema.field("big").type == pa.int64()
    submitted = managed_connection._handle.query.call_args.args[0]
    assert " ".join(submitted.split()) == " ".join(displayed.split())


@pytest.mark.parametrize("empty", [False, True], ids=["values", "typed-empty"])
def test_inline_compiler_produces_an_executable_join_without_registering_data(
    managed_connection, tables, empty
):
    table = ckdata.lake_tbl("types")
    data = pa.table(
        {"id": pa.array([1, 2], type=pa.int32()), "label": ["first", "second"]}
    )
    lookup = ibis.memtable(data.slice(0, 0) if empty else data)
    query = table.left_join(lookup, "id").select("id", "label")
    sql = managed_connection._ibis().compile(query)
    # Execute only our custom inline-table SQL offline. Ordinary boundary tests
    # use mocks rather than a replacement native transport or SQL interpreter.
    with duckdb.connect() as database:
        database.register("types", tables["types"])
        database.execute("CREATE SCHEMA fixtures")
        database.execute("CREATE VIEW fixtures.types AS SELECT * FROM types")
        result = database.sql(sql).to_arrow_table().sort_by("id")
    assert result.to_pylist() == [
        {"id": 1, "label": None if empty else "first"},
        {"id": 2, "label": None if empty else "second"},
    ]
    managed_connection._handle.query.assert_not_called()


def test_ibis_parameters_reach_the_compiler_and_submit_its_sql(
    connection, query_result, monkeypatch
):
    table = connection.table("types")
    parameter = ibis.param("int32")
    query = table.filter(table.id == parameter).aggregate(n=ibis._.count())
    params = {parameter: 1}
    backend = ibis.get_backend(query)
    expected_sql = backend.compile(query, params=params)
    compiler = Mock(wraps=backend.compile)
    monkeypatch.setattr(backend, "compile", compiler)
    query_result["reader"] = pa.table({"n": [1]})
    assert ckdata.collect(query, params=params).equals(query_result["reader"])
    assert compiler.call_args.kwargs["params"] == params
    sql, _, parameters, _ = connection._handle.query.call_args.args
    assert sql == expected_sql
    assert parameters == []


def test_native_parameters_and_timeout_reach_athena_unchanged(connection, query_result):
    query_result["reader"] = pa.table({"value": ["O'Brien"]})
    with ckdata.options(timeout=12):
        result = ckdata.lake_query(
            "SELECT ? AS value", con=connection, params=["O'Brien"]
        )
    assert result.equals(query_result["reader"])
    assert connection._handle.query.call_args.args == (
        "SELECT ? AS value",
        12,
        ["'O''Brien'"],
        False,
    )


def test_arrow_results_retain_native_nested_and_empty_types_without_ibis_recasting(
    connection, query_result
):
    table = connection.table("types")
    actual = pa.table(
        {
            "id": pa.array([1], type=pa.int32()),
            "nested": pa.array([[1, None]], type=pa.list_(pa.int64())),
        }
    )
    query = table.select("id").mutate(
        nested=ibis.literal([1, None], type="array<int32>")
    )
    for expected in (actual, actual.slice(0, 0)):
        query_result["reader"] = expected
        assert ckdata.collect(query).equals(expected, check_metadata=True)


def test_managed_session_reuses_then_reconnects_with_current_settings(
    native_handle, monkeypatch
):
    replacement = Mock(spec=_native.NativeConnection)
    replacement.closed = False
    replacement.info.return_value = native_handle.info.return_value | {
        "workgroup": "second"
    }
    replacement.metadata.side_effect = native_handle.metadata.side_effect
    replacement.query.return_value = native_handle.query.return_value
    replacement.close.side_effect = lambda: setattr(replacement, "closed", True)
    connect = Mock(
        side_effect=[
            Mock(ready=Mock(return_value=True), result=Mock(return_value=handle))
            for handle in (native_handle, replacement)
        ]
    )
    monkeypatch.setattr(_native, "connect", connect)
    ckdata.configure(workgroup="first", schema="fixtures")
    original = ckdata.lake_tbl("types")
    ckdata.lake_tbl("types")
    connect.assert_called_once()
    assert connect.call_args.args[0]["workgroup"] == "first"

    ckdata.lake_disconnect()
    native_handle.close.assert_called_once()
    assert native_handle.closed
    ckdata.configure(workgroup="second")
    current = ckdata.lake_tbl("types")
    assert connect.call_count == 2
    assert connect.call_args.args[0]["workgroup"] == "second"
    with pytest.raises(ckdata.CkDataError, match="connection is closed"):
        ckdata.collect(original)
    ckdata.collect(current)
    native_handle.query.assert_not_called()
    replacement.query.assert_called_once()


def test_supplied_sql_schema_skips_inference(connection):
    query = connection.sql("SELECT id FROM types", schema={"id": "int32"})
    assert query.columns == ("id",)
    connection._handle.query.assert_not_called()


def test_sql_schema_inference_uses_an_empty_query(connection, query_result):
    sql = "SELECT id, big, arr FROM types"
    query_result["reader"] = pa.table(
        {
            "id": pa.array([], type=pa.int32()),
            "big": pa.array([], type=pa.int64()),
            "arr": pa.array([], type=pa.list_(pa.int32())),
        }
    )
    query = connection.sql(sql)
    assert query.schema() == ibis.schema(
        {"id": "int32", "big": "int64", "arr": "array<int32>"}
    )
    connection._handle.query.assert_called_once()
    submitted, _, _, schema_only = connection._handle.query.call_args.args
    assert submitted == sql
    assert schema_only is True


def test_direct_sql_is_forwarded_unchanged(connection):
    sql = "SELECT\n 'first\nsecond' AS value"
    ckdata.lake_query(sql, con=connection)
    assert connection._handle.query.call_args.args[0] == sql


def test_missing_optional_ibis_fails_before_connecting(monkeypatch):
    from ckdata import _connection

    def unavailable(*args):
        raise ModuleNotFoundError("synthetic missing Ibis", name="ibis")

    monkeypatch.setattr(_connection.importlib, "import_module", unavailable)
    with pytest.raises(ImportError, match=r"ckdata\[ibis\]"):
        ckdata.lake_tbl("types")


def test_ibis_arrow_and_pandas_methods_preserve_shapes_and_exact_int64(
    connection, query_result
):
    import pandas as pd

    table = connection.table("types")
    full = query_result["reader"]
    frame = table.execute()
    assert frame["big"].dtype == pd.ArrowDtype(pa.int64())
    assert frame["big"].iloc[0] == 9007199254740993
    query_result["reader"] = full.select(["big"])
    assert table.big.to_pyarrow().to_pylist() == [9007199254740993, None]
    assert table.big.execute().dtype == pd.ArrowDtype(pa.int64())
    query_result["reader"] = pa.table({"count": [2]})
    assert table.count().to_pyarrow().as_py() == 2
    assert table.count().execute() == 2
