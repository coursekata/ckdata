"""Invented Arrow values and mocks at the native boundary; no AWS access."""

from __future__ import annotations

import os
from unittest.mock import Mock

import ckdata
import pyarrow as pa
import pytest
from ckdata import _native, _settings
from ckdata._connection import Connection


def completed(value):
    return Mock(ready=Mock(return_value=True), result=Mock(return_value=value))


@pytest.fixture
def tables():
    return {"types": pa.table(ckdata._arrow_fixture())}


@pytest.fixture
def query_result(tables):
    return {"query_id": "invented", "reader": tables["types"]}


@pytest.fixture
def native_handle(tables, query_result):
    handle = Mock(spec=["closed", "info", "metadata", "query", "close"])
    handle.closed = False
    handle.info.return_value = {
        "account": "111111111111",
        "arn": "arn:aws:sts::111111111111:assumed-role/synthetic-reader/test",
        "region": "us-west-2",
        "schema": "fixtures",
        "workgroup": "synthetic-tests",
    }

    def metadata(schema, table=None):
        if table is None:
            return completed(list(tables))
        types = {
            pa.int32(): "int",
            pa.int64(): "bigint",
            pa.string(): "string",
            pa.timestamp("ms"): "timestamp",
        }
        return completed(
            [(field.name, types[field.type]) for field in tables[table].schema]
        )

    handle.metadata.side_effect = metadata
    handle.query.return_value = completed(query_result)
    handle.close.side_effect = lambda: setattr(handle, "closed", True)
    return handle


@pytest.fixture(autouse=True)
def isolated_options(monkeypatch, request):
    if request.node.get_closest_marker("live"):
        return
    for name in tuple(os.environ):
        if name.startswith(("CKDATA_", "CK_DATALAKE_", "AWS_")):
            monkeypatch.delenv(name)
    monkeypatch.setattr(_settings, "_defaults", {})
    token = _settings._local.set(())

    def no_aws(*args, **kwargs):
        raise AssertionError("unit tests must not connect to AWS")

    monkeypatch.setattr(_native, "connect", no_aws)
    ckdata.lake_disconnect()
    request.addfinalizer(ckdata.lake_disconnect)
    request.addfinalizer(lambda: _settings._local.reset(token))


@pytest.fixture
def connection(native_handle):
    with Connection(native_handle) as con:
        yield con


@pytest.fixture
def managed_connection(monkeypatch, connection):
    from ckdata import _connection

    monkeypatch.setattr(_connection, "_managed", connection)
    ckdata.configure(schema="fixtures")
    return connection
