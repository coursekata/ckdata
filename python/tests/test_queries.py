"""Query composition, native-boundary forwarding and Arrow result shapes."""

import ckdata
import duckdb
import ibis
import pyarrow as pa
import pytest


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


def test_ibis_parameters_are_compiled_while_native_parameters_use_the_athena_api(
    connection, query_result
):
    table = connection.table("types")
    parameter = ibis.param("int32")
    query = table.filter(table.id == parameter).aggregate(n=ibis._.count())
    query_result["reader"] = pa.table({"n": [1]})
    assert ckdata.collect(query, params={parameter: 1}).equals(query_result["reader"])
    sql, _, parameters, _ = connection._handle.query.call_args.args
    assert "COUNT(*)" in sql and " = 1" in sql
    assert parameters == []
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


def test_closed_lazy_query_does_not_adopt_new_managed_session(managed_connection):
    table = ckdata.lake_tbl("types")
    ckdata.lake_disconnect()
    with pytest.raises(ckdata.CkDataError, match="connection is closed"):
        ckdata.collect(table)


def test_supplied_sql_schema_skips_inference_and_direct_sql_is_forwarded_unchanged(
    connection,
):
    query = connection.sql("SELECT id FROM types", schema={"id": "int32"})
    assert query.columns == ("id",)
    connection._handle.query.assert_not_called()
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
