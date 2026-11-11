"""Ibis inspection and projection around the shared Rust column policy."""

import warnings
from unittest.mock import Mock

import ckdata
import duckdb
import ibis
import pyarrow as pa
import pytest


@pytest.fixture
def responses(connection, tables):
    tables["mart_responses"] = pa.table(
        {
            "id": pa.array([1, 1], type=pa.int32()),
            "lrn_type": ["old-a", "old-b"],
            "question_type": ["new-a", "new-b"],
        }
    )
    return connection.table("mart_responses")


def prepare(query):
    return ibis.get_backend(query).prepare(query)[0]


def test_explore_bypasses_inspection_and_audit_preserves_the_query(
    responses, monkeypatch
):
    from ckdata import _columns

    with ckdata.options(mode="audit"), pytest.warns(ckdata.ColumnWarning):
        assert prepare(responses) is responses
    monkeypatch.setattr(
        _columns,
        "_columns_read",
        lambda *args: pytest.fail("explore must bypass inspection"),
    )
    with ckdata.options(mode="explore"):
        assert prepare(responses) is responses


def test_publish_projects_after_filter_and_distinct_without_changing_rows(
    responses, tables
):
    queries = [
        (
            responses.filter(responses.lrn_type == "old-a").select("id", "lrn_type"),
            [{"id": 1}],
        ),
        (responses.select("id", "lrn_type").distinct(), [{"id": 1}, {"id": 1}]),
    ]
    with duckdb.connect() as database:
        database.register("mart_responses", tables["mart_responses"])
        database.execute("CREATE SCHEMA fixtures")
        database.execute(
            "CREATE VIEW fixtures.mart_responses AS SELECT * FROM mart_responses"
        )
        for query, expected in queries:
            with ckdata.options(mode="publish"), pytest.warns(ckdata.ColumnWarning):
                projected = prepare(query)
            assert projected.columns == ("id",)
            sql = ibis.get_backend(responses).compile(projected)
            assert database.sql(sql).to_arrow_table().to_pylist() == expected


def test_renamed_and_derived_outputs_warn_but_remain_like_r(responses):
    for query in [
        responses.select("id", renamed=responses.lrn_type),
        responses.select("id", derived=responses.lrn_type.upper()),
    ]:
        with ckdata.options(mode="publish"), pytest.warns(ckdata.ColumnWarning):
            assert prepare(query) is query


def test_pandas_execution_receives_the_published_projection(
    responses, connection, query_result, monkeypatch
):
    backend = ibis.get_backend(responses)
    compile_query = backend.compile
    compiler = Mock(wraps=compile_query)
    monkeypatch.setattr(backend, "compile", compiler)
    query_result["reader"] = pa.table({"id": [1], "question_type": ["new"]})
    with ckdata.options(mode="publish"), pytest.warns(ckdata.ColumnWarning):
        assert tuple(responses.execute().columns) == ("id", "question_type")
    sql = connection._handle.query.call_args.args[0]
    compiled = compiler.call_args
    assert compiled.args[0].columns == ("id", "question_type")
    assert sql == compile_query(*compiled.args, **compiled.kwargs)


def test_explicit_allow_and_replacement_alias_do_not_warn(responses):
    with warnings.catch_warnings():
        warnings.simplefilter("error", ckdata.ColumnWarning)
        with ckdata.options(mode="publish", allow={"mart_responses": ["lrn_type"]}):
            assert prepare(responses) is responses
        with ckdata.options(mode="audit"):
            shim = responses.select("id", lrn_type=responses.question_type)
            assert prepare(shim) is shim


def test_all_flagged_outputs_fail_before_execution_and_raw_sql_bypasses(
    responses, connection
):
    with ckdata.options(mode="publish"):
        with pytest.raises(ckdata.ColumnPolicyError, match="Every output column"):
            ckdata.collect(responses.select("lrn_type"))
        connection._handle.query.assert_not_called()
        ckdata.lake_query("SELECT lrn_type FROM mart_responses", con=connection)
        connection._handle.query.assert_called_once()


def test_staging_columns_except_shared_published_keys_are_removed(connection, tables):
    tables["stg_events"] = pa.table(
        {"id": pa.array([1], type=pa.int32()), "raw": ["invented"]}
    )
    tables["mart_events"] = pa.table(
        {"id": pa.array([1], type=pa.int32()), "published": ["ready"]}
    )
    staging, published = connection.table("stg_events"), connection.table("mart_events")
    query = staging.join(published, "id")
    with ckdata.options(mode="publish"), pytest.warns(ckdata.ColumnWarning) as caught:
        assert prepare(query).columns == ("id", "published")
    assert "`id` (stg_events)" not in str(caught[0].message)
