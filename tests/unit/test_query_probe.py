"""Offline result-path and live-gate tests using entirely invented data."""

import builtins
import importlib
import io
from datetime import datetime
from decimal import Decimal
from unittest.mock import Mock

import pyarrow as pa
import pyarrow.parquet as pq
import pytest

from scripts import query_probe as probe


@pytest.fixture
def table():
    rows = [
        {
            "id": 1,
            "s_null": None,
            "s_empty": "",
            "big": 9007199254740993,
            "dec": Decimal("1.10"),
            "ts": datetime(2026, 10, 16, 12, 34, 56, 789000),
            "arr": [1, 2],
        },
        {
            "id": 2,
            "s_null": "a",
            "s_empty": "b",
            "big": None,
            "dec": None,
            "ts": None,
            "arr": None,
        },
    ]
    schema = pa.schema(
        [
            ("id", pa.int32()),
            ("s_null", pa.string()),
            ("s_empty", pa.string()),
            ("big", pa.int64()),
            ("dec", pa.decimal128(3, 2)),
            ("ts", pa.timestamp("ms")),
            ("arr", pa.list_(pa.int32())),
        ]
    )
    return pa.Table.from_pylist(rows, schema=schema)


@pytest.fixture
def deployment():
    return {
        "format_version": 1,
        "purpose": "ckdata-synthetic-tests",
        "account_id": "000000000000",
        "region": "us-west-2",
        "bucket": "ck-datalake-tst-ckdata-fixtures-000000000000",
        "database": "ck_datalake_tst_ckdata_fixtures",
        "table": "types",
        "fixture_keys": [
            "fixtures/types/part-1.parquet",
            "fixtures/types/part-2.parquet",
        ],
        "operator_role_arns": ["arn:aws:iam::000000000000:role/operator"],
        "seats": {
            seat: {
                "role_arn": f"arn:aws:iam::000000000000:role/ck-datalake-tst-ckdata-{seat}-reader",
                "source_role_arn": "arn:aws:iam::000000000000:role/ck-datalake-tst-ckdata-source"
                if seat == "research"
                else None,
                "workgroup": f"ck-datalake-tst-ckdata-{seat}",
                "output_location": f"s3://ck-datalake-tst-ckdata-fixtures-000000000000/results/{seat}/",
            }
            for seat in ("internal", "research")
        },
    }


class FakeServices:
    def __init__(self, table):
        self.table = table
        self.queries = {}
        self.objects = {}
        self.metadata_query_counts = []
        self.empty_manifest = False
        self.cancelled = []

    def get_work_group(self, **kwargs):
        return {
            "WorkGroup": {
                "Configuration": {
                    "EnforceWorkGroupConfiguration": True,
                    "EngineVersion": {
                        "SelectedEngineVersion": "Athena engine version 3"
                    },
                    "ResultConfiguration": {
                        "OutputLocation": "s3://synthetic/results/",
                        "EncryptionConfiguration": {"EncryptionOption": "SSE_S3"},
                    },
                }
            }
        }

    def start_query_execution(self, **kwargs):
        sql = kwargs["QueryString"]
        query_id = f"synthetic-{len(self.queries)}"
        manifest_uri = f"s3://synthetic/results/{query_id}-manifest.csv"
        output_uri = sql.split("TO '", 1)[1].split("'", 1)[0] + "part.parquet"
        data = self.table
        data = data.slice(0, 0) if "WHERE false" in sql else data
        stream = io.BytesIO()
        pq.write_table(data, stream)
        self.objects[output_uri] = stream.getvalue()
        self.objects[manifest_uri] = (output_uri + "\n").encode()
        if self.empty_manifest and "WHERE false" in sql:
            self.objects[manifest_uri] = b""
        self.queries[query_id] = {
            "QueryExecutionId": query_id,
            "WorkGroup": kwargs["WorkGroup"],
            "ResultConfiguration": {
                "OutputLocation": "s3://synthetic/results/",
                "EncryptionConfiguration": {"EncryptionOption": "SSE_S3"},
            },
            "Status": {"State": "SUCCEEDED"},
            "Statistics": {"DataManifestLocation": manifest_uri},
        }
        return {"QueryExecutionId": query_id}

    def get_query_execution(self, **kwargs):
        return {"QueryExecution": self.queries[kwargs["QueryExecutionId"]]}

    def stop_query_execution(self, **kwargs):
        self.cancelled.append(kwargs["QueryExecutionId"])

    def get_object(self, **kwargs):
        probe.require(
            kwargs["ExpectedBucketOwner"] == "000000000000", "expected object owner"
        )
        uri = f"s3://{kwargs['Bucket']}/{kwargs['Key']}"
        return {
            "Body": io.BytesIO(self.objects[uri]),
            "ServerSideEncryption": "AES256",
        }

    def list_table_metadata(self, **kwargs):
        probe.require(
            1 <= kwargs["MaxResults"] <= 50,
            "Athena metadata page size is outside its API limit",
        )
        self.metadata_query_counts.append(len(self.queries))
        return {"TableMetadataList": [{"Name": "synthetic_table"}]}

    def get_table_metadata(self, **kwargs):
        self.metadata_query_counts.append(len(self.queries))
        return {"TableMetadata": {"Columns": [{"Name": "synthetic_column"}]}}


@pytest.fixture
def services(table):
    return FakeServices(table)


@pytest.fixture
def query_probe(services):
    return probe.Probe(services, services, "synthetic-workgroup", "000000000000")


def test_manifest_paths_and_empty_results():
    target = "s3://synthetic/results/probe/"
    assert probe.manifest_files(
        b'"s3://synthetic/results/probe/part.parquet"\n', target
    ) == [target + "part.parquet"]
    assert probe.manifest_files(b"", target) == []


@pytest.mark.parametrize(
    "path", ["s3://other/results/probe/file", "s3://synthetic/results/other/file"]
)
def test_manifest_rejects_other_prefixes(path):
    with pytest.raises(RuntimeError):
        probe.manifest_files((path + "\n").encode(), "s3://synthetic/results/probe/")


def test_kms_object_requires_matching_full_key_arn():
    key = "arn:aws:kms:us-west-2:000000000000:key/00000000-0000-4000-8000-000000000001"
    response = {"ServerSideEncryption": "aws:kms", "SSEKMSKeyId": key}
    expected = {"EncryptionOption": "SSE_KMS", "KmsKey": key}
    probe.encryption_matches(response, expected)
    response["SSEKMSKeyId"] = (
        "arn:aws:kms:us-west-2:000000000000:key/00000000-0000-4000-8000-000000000002"
    )
    with pytest.raises(RuntimeError, match="different KMS key"):
        probe.encryption_matches(response, expected)


@pytest.mark.parametrize("unit", ["ms", "us", "ns"])
def test_naive_timestamp_accepts_supported_storage_units(table, unit):
    probe.check_types(
        table.cast(table.schema.set(5, pa.field("ts", pa.timestamp(unit))))
    )


@pytest.mark.parametrize(
    "name,changed_type",
    [
        ("id", pa.int64()),
        ("dec", pa.decimal128(3, 1)),
        ("arr", pa.list_(pa.int64())),
        ("ts", pa.timestamp("us", "UTC")),
    ],
)
def test_schema_drift_is_rejected(table, name, changed_type):
    changed = table.cast(
        table.schema.set(
            table.schema.get_field_index(name), pa.field(name, changed_type)
        )
    )
    with pytest.raises(RuntimeError):
        probe.check_types(changed)


def test_unload_values_and_schema_bearing_empty_result(query_probe, table):
    populated = query_probe.unload(probe.PROBE, "offline-literal")
    probe.check_types(populated)
    assert populated.equals(table)
    empty = query_probe.unload(
        probe.PROBE + " WHERE false", "offline-empty", allow_empty=True
    )
    assert empty.schema == populated.schema
    assert empty.to_pylist() == []


def test_catalog_discovery_submits_no_query(services, query_probe):
    query_probe.table_access("synthetic_database", "synthetic_table")
    assert services.metadata_query_counts == [0, 0]
    assert len(services.queries) == 1  # Only the subsequent invented-table check.


def test_file_free_empty_manifest(services, query_probe):
    services.empty_manifest = True
    assert (
        query_probe.unload(
            probe.PROBE + " WHERE false", "offline-no-files", allow_empty=True
        )
        is None
    )


def test_timeout_requests_remote_stop(services, query_probe, monkeypatch):
    clock = iter([0, 181])
    with monkeypatch.context() as context:
        context.setattr(probe.time, "monotonic", lambda: next(clock))
        with pytest.raises(TimeoutError):
            query_probe.execute(
                "UNLOAD (SELECT 1) TO 's3://synthetic/results/timeout/' WITH (format='PARQUET')"
            )
    assert len(services.cancelled) == 1


@pytest.mark.parametrize("stop_fails", [False, True])
def test_poll_failure_requests_stop_without_masking_original(
    services, query_probe, monkeypatch, stop_fails
):
    original = OSError("synthetic polling failure")
    monkeypatch.setattr(services, "get_query_execution", Mock(side_effect=original))
    stop = Mock(side_effect=OSError("stop failed") if stop_fails else None)
    monkeypatch.setattr(services, "stop_query_execution", stop)
    with pytest.raises(OSError) as error:
        query_probe.execute(
            "UNLOAD (SELECT 1) TO 's3://synthetic/results/failure/' WITH (format='PARQUET')"
        )
    assert error.value is original
    stop.assert_called_once()


@pytest.mark.parametrize("drift", ["missing_cutoff", "wrong_cutoff", "wrong_output"])
def test_workgroup_drift_rejects_before_submission(services, monkeypatch, drift):
    config = services.get_work_group()["WorkGroup"]["Configuration"]
    config["BytesScannedCutoffPerQuery"] = 10 * 1024 * 1024
    config["ResultConfiguration"]["ExpectedBucketOwner"] = "000000000000"
    if drift == "missing_cutoff":
        config.pop("BytesScannedCutoffPerQuery")
    elif drift == "wrong_cutoff":
        config["BytesScannedCutoffPerQuery"] += 1
    else:
        config["ResultConfiguration"]["OutputLocation"] = "s3://other/results/"
    monkeypatch.setattr(
        services, "get_work_group", lambda **_: {"WorkGroup": {"Configuration": config}}
    )
    with pytest.raises(RuntimeError):
        probe.Probe(
            services,
            services,
            "synthetic-workgroup",
            "000000000000",
            "s3://synthetic/results/",
        )
    assert not services.queries


@pytest.mark.parametrize(
    "account,role,expected",
    [
        ("000000000000", "reader", True),
        ("000000000000", "admin", False),
        ("111111111111", "reader", False),
    ],
)
def test_identity_requires_exact_role_and_account(account, role, expected):
    identity = {
        "Account": account,
        "Arn": f"arn:aws:sts::{account}:assumed-role/{role}/test",
    }
    assert (
        probe.role_matches(identity, "arn:aws:iam::000000000000:role/reader")
        is expected
    )


def test_deployment_accepts_exact_cross_account_operator(deployment):
    probe.validate_deployment(deployment)
    deployment["operator_role_arns"] = ["arn:aws:iam::111111111111:role/operator"]
    probe.validate_deployment(deployment)


@pytest.mark.parametrize(
    "field,value",
    [
        ("bucket", "example-tst-fixtures"),
        ("bucket", "ck-datalake-tst-ckdata-fixtures-111111111111"),
        ("database", "example_tst_fixtures"),
        ("region", "us-east-1"),
        ("table", "other"),
    ],
)
def test_deployment_rejects_namespace_drift(deployment, field, value):
    deployment[field] = value
    with pytest.raises(RuntimeError):
        probe.validate_deployment(deployment)


@pytest.mark.parametrize(
    "seat,field",
    [
        ("internal", "workgroup"),
        ("research", "role_arn"),
        ("research", "source_role_arn"),
    ],
)
def test_deployment_rejects_near_miss_role_and_workgroup_names(deployment, seat, field):
    deployment["seats"][seat][field] += "-near-miss"
    with pytest.raises(RuntimeError):
        probe.validate_deployment(deployment)


def test_deployment_rejects_cross_account_runtime_role(deployment):
    role = deployment["seats"]["research"]["role_arn"]
    deployment["seats"]["research"]["role_arn"] = role.replace(
        "::000000000000:", "::111111111111:", 1
    )
    with pytest.raises(RuntimeError):
        probe.validate_deployment(deployment)


def test_expected_denial_and_unexpected_success_close_the_body():
    denied = RuntimeError("synthetic denial")
    denied.response = {"Error": {"Code": "AccessDenied"}}
    probe.expect_denied(Mock(side_effect=denied), "offline denial classifier")
    body = io.BytesIO(b"synthetic")
    with pytest.raises(RuntimeError):
        probe.expect_denied(lambda: {"Body": body}, "unexpected success")
    assert body.closed


def test_help_no_live_and_invalid_contract_do_not_import_aws_sdk(monkeypatch):
    ordinary_import = builtins.__import__

    def without_sdk(name, *args, **kwargs):
        if name.split(".")[0] in {"boto3", "botocore"}:
            pytest.fail(f"SDK import before validation: {name}")
        return ordinary_import(name, *args, **kwargs)

    with monkeypatch.context() as context:
        context.setattr(builtins, "__import__", without_sdk)
        importlib.reload(probe)
        context.setattr("sys.argv", ["query_probe", "--help"])
        with pytest.raises(SystemExit) as error:
            probe.main()
        assert error.value.code == 0
        context.setattr("sys.argv", ["query_probe"])
        with pytest.raises(RuntimeError, match="live mode requires"):
            probe.main()
        with pytest.raises((KeyError, RuntimeError)):
            probe.run_live({})
