# /// script
# requires-python = ">=3.11"
# dependencies = ["boto3>=1.40,<2", "pyarrow>=26,<27"]
# [tool.uv]
# exclude-newer = "7 days"
# ///
"""Check the independently deployed synthetic ckdata environment.

Run --self-test for synthetic, offline checks; that mode constructs no AWS clients.
Live mode requires --run-live and a deployment contract. Prints no data rows.
"""

import argparse
import csv
import io
import json
import re
import sys
import time
import uuid
from datetime import datetime
from decimal import Decimal
from functools import partial
from pathlib import Path
from urllib.parse import urlsplit

PROBE = """SELECT * FROM (VALUES
(1, CAST(NULL AS VARCHAR), '', BIGINT '9007199254740993',
 DECIMAL '1.10', TIMESTAMP '2026-10-16 12:34:56.789', ARRAY[1, 2]),
(2, 'a', 'b', NULL, NULL, NULL, NULL))
AS t(id, s_null, s_empty, big, dec, ts, arr)"""


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def s3_location(uri):
    parsed = urlsplit(uri)
    require(parsed.scheme == "s3" and bool(parsed.netloc), "invalid S3 location")
    require(not parsed.query and not parsed.fragment, "noncanonical S3 location")
    return parsed.netloc, parsed.path.lstrip("/")


def confined(uri, prefix):
    bucket, key = s3_location(uri)
    owner_bucket, owner_prefix = s3_location(prefix)
    require(
        bucket == owner_bucket and key.startswith(owner_prefix.rstrip("/") + "/"),
        "result location is outside this probe's allowed prefix",
    )
    return bucket, key


def manifest_files(raw, target):
    files = []
    for row in csv.reader(io.StringIO(raw.decode("utf-8-sig"))):
        if not row or not any(row):
            continue
        require(
            len(row) == 1 and row[0].startswith("s3://"),
            "unrecognized manifest format; no data objects were fetched",
        )
        confined(row[0], target)
        files.append(row[0])
    require(
        len(files) <= 32 and len(set(files)) == len(files),
        "unexpected manifest size or duplicate file entries",
    )
    return files


def encryption_matches(response, expected):
    mode = expected.get("EncryptionOption")
    permitted = {"SSE_KMS": {"aws:kms", "aws:kms:dsse"}, "SSE_S3": {"AES256"}}
    require(
        mode in permitted, "workgroup has no supported server-side encryption setting"
    )
    require(
        response.get("ServerSideEncryption") in permitted[mode],
        "object encryption differs from the workgroup setting",
    )
    key = expected.get("KmsKey", "")
    if mode == "SSE_KMS":
        require(
            ":key/" in key, "use a workgroup with a full KMS key ARN for this check"
        )
        require(response.get("SSEKMSKeyId") == key, "object uses a different KMS key")


def quote_identifier(identifier):
    return '"' + identifier.replace('"', '""') + '"'


def check_types(table):
    import pyarrow as pa

    timestamp = table.schema.field("ts").type
    require(
        pa.types.is_timestamp(timestamp)
        and timestamp.tz is None
        and timestamp.unit in {"ms", "us", "ns"},
        "timestamp type, precision or timezone changed",
    )
    expected = pa.schema(
        [
            ("id", pa.int32()),
            ("s_null", pa.string()),
            ("s_empty", pa.string()),
            ("big", pa.int64()),
            ("dec", pa.decimal128(3, 2)),
            ("ts", timestamp),
            ("arr", pa.list_(pa.int32())),
        ]
    )
    require(
        table.schema.equals(expected, check_metadata=False),
        f"literal Arrow schema changed: {table.schema}",
    )
    rows = sorted(table.to_pylist(), key=lambda row: row["id"])
    require(len(rows) == 2, "literal probe did not return exactly two rows")
    require(
        pa.types.is_int64(table.schema.field("big").type), "bigint is not Arrow int64"
    )
    require(
        rows[0]
        == {
            "id": 1,
            "s_null": None,
            "s_empty": "",
            "big": 9007199254740993,
            "dec": Decimal("1.10"),
            "ts": datetime(2026, 10, 16, 12, 34, 56, 789000),
            "arr": [1, 2],
        },
        "literal values or types changed in the result path",
    )
    require(
        rows[1]
        == {
            "id": 2,
            "s_null": "a",
            "s_empty": "b",
            "big": None,
            "dec": None,
            "ts": None,
            "arr": None,
        },
        "literal NULL values changed",
    )


class Probe:
    def __init__(
        self,
        athena,
        s3,
        workgroup,
        account,
        expected_output=None,
        database="synthetic_database",
    ):
        self.athena, self.s3 = athena, s3
        self.workgroup, self.account = workgroup, account
        self.database = database
        config = athena.get_work_group(WorkGroup=workgroup)["WorkGroup"][
            "Configuration"
        ]
        require(
            config.get("EnforceWorkGroupConfiguration") is True,
            "workgroup controls are not enforced",
        )
        require(
            config.get("EngineVersion", {}).get("SelectedEngineVersion")
            == "Athena engine version 3",
            "unexpected Athena engine",
        )
        if expected_output is not None:
            require(
                config.get("BytesScannedCutoffPerQuery") == 10 * 1024 * 1024,
                "workgroup scan cutoff differs from the 10 MiB fixture contract",
            )
        result = config["ResultConfiguration"]
        self.output = result["OutputLocation"].rstrip("/") + "/"
        if expected_output is not None:
            require(
                self.output == expected_output,
                "workgroup output differs from the test deployment",
            )
            require(
                result.get("ExpectedBucketOwner") == account
                and result.get("EncryptionConfiguration")
                == {"EncryptionOption": "SSE_S3"},
                "workgroup owner or encryption differs from the fixture contract",
            )
        s3_location(self.output)
        self.encryption = result.get("EncryptionConfiguration", {})

    def execute(self, sql):
        query_id = self.athena.start_query_execution(
            QueryString=sql,
            WorkGroup=self.workgroup,
            ClientRequestToken=uuid.uuid4().hex,
            ResultReuseConfiguration={
                "ResultReuseByAgeConfiguration": {"Enabled": False}
            },
            QueryExecutionContext={
                "Catalog": "AwsDataCatalog",
                "Database": self.database,
            },
        )["QueryExecutionId"]
        terminal = False
        try:
            deadline = time.monotonic() + 180
            while time.monotonic() < deadline:
                query = self.athena.get_query_execution(QueryExecutionId=query_id)[
                    "QueryExecution"
                ]
                state = query["Status"]["State"]
                terminal = state in {"SUCCEEDED", "FAILED", "CANCELLED"}
                if state == "SUCCEEDED":
                    require(
                        query.get("WorkGroup") == self.workgroup,
                        "query used another workgroup",
                    )
                    result = query.get("ResultConfiguration", {})
                    require(
                        result.get("OutputLocation", "").startswith(self.output),
                        "query output escaped the test prefix",
                    )
                    require(
                        result.get("EncryptionConfiguration") == self.encryption,
                        "query encryption differs from its workgroup",
                    )
                    return query
                require(
                    state not in {"FAILED", "CANCELLED"},
                    f"Athena query {state}; query_id={query_id}; inspect its reason in Athena",
                )
                time.sleep(0.5)
            raise TimeoutError(f"query timed out; query_id={query_id}")
        except BaseException:
            # Once a query exists, every abnormal nonterminal exit needs a stop.
            if not terminal:
                try:
                    self.athena.stop_query_execution(QueryExecutionId=query_id)
                except (Exception, KeyboardInterrupt):  # noqa: BLE001 -- preserve the original failure after a best-effort stop
                    # Keep the polling/interrupt failure as the original exception.
                    print(
                        f"WARN cancellation unconfirmed; query_id={query_id}",
                        file=sys.stderr,
                    )
            raise

    def object_bytes(self, uri, prefix):
        bucket, key = confined(uri, prefix)
        response = self.s3.get_object(
            Bucket=bucket, Key=key, ExpectedBucketOwner=self.account
        )
        body = response["Body"]
        try:
            encryption_matches(response, self.encryption)
            data = body.read(8 * 1024 * 1024 + 1)
        finally:
            body.close()
        require(
            len(data) <= 8 * 1024 * 1024, "probe object exceeds the expected small size"
        )
        return data

    def unload(self, select, label, allow_empty=False):
        import pyarrow as pa
        import pyarrow.parquet as pq

        target = self.output + "ckdata-result-path-check/" + uuid.uuid4().hex + "/"
        query = self.execute(
            f"UNLOAD ({select}) TO '{target.replace(chr(39), chr(39) * 2)}' WITH (format='PARQUET', compression='SNAPPY')"
        )
        manifest = query.get("Statistics", {}).get("DataManifestLocation")
        if not manifest:
            require(
                allow_empty, "successful nonempty UNLOAD returned no manifest location"
            )
            print("INFO empty-result: manifest=absent files=unknown schema=unavailable")
            return None
        raw = self.object_bytes(manifest, self.output)
        files = manifest_files(raw, target)
        tables = [
            pq.read_table(pa.BufferReader(self.object_bytes(uri, target)))
            for uri in files
        ]
        if not tables:
            require(allow_empty, "nonempty UNLOAD manifest lists no files")
            print("INFO empty-result: manifest=present files=0 schema=unavailable")
            return None
        table = pa.concat_tables(tables)
        if allow_empty:
            require(table.num_rows == 0, "zero-row query returned rows")
            print(
                f"INFO empty-result: manifest=present files={len(files)} schema=available"
            )
        else:
            print(f"PASS {label}: manifest, Parquet read and workgroup encryption")
        return table

    def table_access(self, database, table_name):
        listed = self.athena.list_table_metadata(
            CatalogName="AwsDataCatalog", DatabaseName=database, MaxResults=50
        )["TableMetadataList"]
        require(
            any(item["Name"] == table_name for item in listed),
            "the named synthetic table was not discoverable",
        )
        metadata = self.athena.get_table_metadata(
            CatalogName="AwsDataCatalog", DatabaseName=database, TableName=table_name
        )["TableMetadata"]
        require(bool(metadata.get("Columns")), "table metadata has no columns")
        print("PASS table/column metadata available without submitting a query")
        select = (
            f"SELECT * FROM {quote_identifier(database)}.{quote_identifier(table_name)}"
        )
        check_types(self.unload(select, "invented-fixture"))
        print("PASS named invented fixture preserved all expected values and types")


def validate_deployment(deployment):
    require(deployment.get("format_version") == 1, "unsupported deployment contract")
    require(
        deployment.get("purpose") == "ckdata-synthetic-tests",
        "not a synthetic test deployment",
    )
    account = deployment.get("account_id", "")
    require(re.fullmatch(r"[0-9]{12}", account), "invalid test account")
    require(
        deployment.get("region") == "us-west-2",
        "fixture region must be us-west-2",
    )
    bucket = deployment.get("bucket", "")
    require(
        bucket == f"ck-datalake-tst-ckdata-fixtures-{account}",
        "bucket is not in the test namespace",
    )
    require(
        deployment.get("database") == "ck_datalake_tst_ckdata_fixtures",
        "database is not in the test namespace",
    )
    require(
        deployment.get("table") == "types",
        "only the invented types fixture is supported",
    )
    require(
        deployment.get("fixture_keys")
        == ["fixtures/types/part-1.parquet", "fixtures/types/part-2.parquet"],
        "unexpected fixture objects",
    )
    operators = deployment.get("operator_role_arns", [])
    role_pattern = r"arn:aws:iam::[0-9]{12}:role/[A-Za-z0-9+=,.@_/-]+"
    require(
        isinstance(operators, list)
        and bool(operators)
        and all(re.fullmatch(role_pattern, arn) for arn in operators),
        "operator roles must be exact IAM role ARNs",
    )
    require(
        set(deployment.get("seats", {})) == {"internal", "research"},
        "both test seats are required",
    )
    for seat, config in deployment["seats"].items():
        require(
            config.get("role_arn")
            == f"arn:aws:iam::{account}:role/ck-datalake-tst-ckdata-{seat}-reader",
            "unexpected synthetic reader role",
        )
        expected_source = (
            f"arn:aws:iam::{account}:role/ck-datalake-tst-ckdata-source"
            if seat == "research"
            else None
        )
        require(
            config.get("source_role_arn") == expected_source,
            "unexpected synthetic source role",
        )
        require(
            config.get("workgroup") == f"ck-datalake-tst-ckdata-{seat}",
            "unexpected synthetic workgroup",
        )
        require(
            config.get("output_location") == f"s3://{bucket}/results/{seat}/",
            "unexpected test output prefix",
        )
    return deployment


def role_matches(identity, arn):
    return identity.get("Account") == arn.split(":")[4] and identity.get(
        "Arn", ""
    ).startswith(
        f"arn:aws:sts::{arn.split(':')[4]}:assumed-role/{arn.rsplit('/', 1)[-1]}/"
    )


def assume(session, arn, label, config, session_factory):
    print(f"INFO credentials: assuming {label}")
    credentials = session.client("sts", config=config).assume_role(
        RoleArn=arn, RoleSessionName=f"ckdata-test-{label}", DurationSeconds=3600
    )["Credentials"]
    target = session_factory(
        aws_access_key_id=credentials["AccessKeyId"],
        aws_secret_access_key=credentials["SecretAccessKey"],
        aws_session_token=credentials["SessionToken"],
        region_name=session.region_name,
    )
    require(
        role_matches(target.client("sts", config=config).get_caller_identity(), arn),
        "unexpected assumed role; no data clients were created",
    )
    return target


def expect_denied(operation, label):
    try:
        response = operation()
    except Exception as error:
        code = getattr(error, "response", {}).get("Error", {}).get("Code")
        if code in {"AccessDenied", "AccessDeniedException"}:
            print(f"PASS denied: {label}")
            return
        raise
    if isinstance(response, dict) and "Body" in response:
        response["Body"].close()
    raise RuntimeError(f"unexpected access: {label}; no response body was read")


def run_live(deployment):
    deployment = validate_deployment(deployment)
    import boto3
    from botocore.config import Config

    config = Config(
        connect_timeout=10,
        read_timeout=30,
        retries={"mode": "standard", "max_attempts": 3},
    )
    base = boto3.Session(region_name=deployment["region"])
    print("INFO credentials: checking operator identity")
    identity = base.client("sts", config=config).get_caller_identity()
    require(
        any(role_matches(identity, arn) for arn in deployment["operator_role_arns"]),
        "source identity is not an approved test operator",
    )
    for seat in ("internal", "research"):
        settings = deployment["seats"][seat]
        source = base
        if settings["source_role_arn"]:
            source = assume(
                base,
                settings["source_role_arn"],
                "research-source",
                config,
                boto3.Session,
            )
            expect_denied(
                partial(
                    source.client("athena", config=config).get_work_group,
                    WorkGroup=settings["workgroup"],
                ),
                "source role has no direct Athena access",
            )
        reader = assume(
            source, settings["role_arn"], f"{seat}-reader", config, boto3.Session
        )
        print(f"PASS {seat}: exact test identity")
        athena, s3 = (
            reader.client("athena", config=config),
            reader.client("s3", config=config),
        )
        probe = Probe(
            athena,
            s3,
            settings["workgroup"],
            deployment["account_id"],
            settings["output_location"],
            deployment["database"],
        )
        literal = probe.unload(PROBE, f"{seat}-literal")
        check_types(literal)
        empty = probe.unload(PROBE + " WHERE false", "empty-result", allow_empty=True)
        if empty is not None:
            require(
                empty.schema.equals(literal.schema, check_metadata=False),
                "empty Parquet schema differs",
            )
        probe.table_access(deployment["database"], deployment["table"])
        other = deployment["seats"]["research" if seat == "internal" else "internal"][
            "workgroup"
        ]
        expect_denied(
            partial(athena.get_work_group, WorkGroup=other), "other test workgroup"
        )
        # The successful exact two-row fixture query above proves these files exist.
        expect_denied(
            partial(
                s3.get_object,
                Bucket=deployment["bucket"],
                Key=deployment["fixture_keys"][0],
                ExpectedBucketOwner=deployment["account_id"],
            ),
            "direct fixture-object read",
        )
        print(f"PASS {seat}: synthetic result path and access boundaries")


def self_test():
    import pyarrow as pa
    import pyarrow.parquet as pq

    target = "s3://synthetic/results/probe/"
    files = manifest_files(b'"s3://synthetic/results/probe/part.parquet"\n', target)
    require(len(files) == 1, "CSV manifest parsing")
    require(manifest_files(b"", target) == [], "empty manifest parsing")
    for invalid in (
        b"s3://other/results/probe/file\n",
        b"s3://synthetic/results/other/file\n",
    ):
        try:
            manifest_files(invalid, target)
        except RuntimeError:
            pass
        else:
            raise RuntimeError("manifest confinement test failed")
    key = "arn:aws:kms:us-west-2:000000000000:key/synthetic"
    encryption_matches(
        {"ServerSideEncryption": "aws:kms", "SSEKMSKeyId": key},
        {"EncryptionOption": "SSE_KMS", "KmsKey": key},
    )
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
    table = pa.Table.from_pylist(rows, schema=schema)
    # Athena INT96 commonly reads as ns; fixture files can retain ms or us.
    # Every supported representation must preserve the same exact instant.
    for unit in ("ms", "us", "ns"):
        check_types(table.cast(schema.set(5, pa.field("ts", pa.timestamp(unit)))))
    for name, changed_type in (
        ("id", pa.int64()),
        ("dec", pa.decimal128(3, 1)),
        ("arr", pa.list_(pa.int64())),
        ("ts", pa.timestamp("us", "UTC")),
    ):
        changed = table.cast(
            schema.set(schema.get_field_index(name), pa.field(name, changed_type))
        )
        try:
            check_types(changed)
        except RuntimeError:
            pass
        else:
            raise RuntimeError("schema drift was accepted")
    buffer = io.BytesIO()
    pq.write_table(table, buffer)
    check_types(pq.read_table(pa.BufferReader(buffer.getvalue())))
    require(
        pq.read_table(pa.BufferReader(buffer.getvalue())).slice(0, 0).num_rows == 0,
        "empty Arrow table check",
    )

    class FakeServices:
        def __init__(self):
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
            data = table
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
            require(
                kwargs["ExpectedBucketOwner"] == "000000000000", "expected object owner"
            )
            uri = f"s3://{kwargs['Bucket']}/{kwargs['Key']}"
            return {
                "Body": io.BytesIO(self.objects[uri]),
                "ServerSideEncryption": "AES256",
            }

        def list_table_metadata(self, **kwargs):
            require(
                1 <= kwargs["MaxResults"] <= 50,
                "Athena metadata page size is outside its API limit",
            )
            self.metadata_query_counts.append(len(self.queries))
            return {"TableMetadataList": [{"Name": "synthetic_table"}]}

        def get_table_metadata(self, **kwargs):
            self.metadata_query_counts.append(len(self.queries))
            return {"TableMetadata": {"Columns": [{"Name": "synthetic_column"}]}}

    fake = FakeServices()
    probe = Probe(fake, fake, "synthetic-workgroup", "000000000000")
    check_types(probe.unload(PROBE, "offline-literal"))
    require(
        probe.unload(PROBE + " WHERE false", "offline-empty", allow_empty=True).num_rows
        == 0,
        "zero-row service path",
    )
    probe.table_access("synthetic_database", "synthetic_table")
    require(fake.metadata_query_counts == [2, 2], "metadata calls submitted a query")
    require(len(fake.queries) == 3, "unexpected extra queries")
    fake.empty_manifest = True
    require(
        probe.unload(PROBE + " WHERE false", "offline-no-files", allow_empty=True)
        is None,
        "empty manifest representation",
    )
    from unittest.mock import patch

    with patch("time.monotonic", side_effect=[0, 181]):
        try:
            probe.execute(
                "UNLOAD (SELECT 1) TO 's3://synthetic/results/timeout/' WITH (format='PARQUET')"
            )
        except TimeoutError:
            require(len(fake.cancelled) == 1, "timeout did not cancel its query")
        else:
            raise RuntimeError("timeout was not reported")
    for stop_fails in (False, True):
        original = OSError("synthetic polling failure")
        with (
            patch.object(fake, "get_query_execution", side_effect=original),
            patch.object(
                fake,
                "stop_query_execution",
                side_effect=OSError("stop failed") if stop_fails else None,
            ) as stop,
        ):
            try:
                probe.execute(
                    "UNLOAD (SELECT 1) TO 's3://synthetic/results/failure/' WITH (format='PARQUET')"
                )
            except OSError as error:
                require(error is original, "cancellation masked the polling failure")
                require(stop.call_count == 1, "polling failure did not stop the query")
            else:
                raise RuntimeError("polling error was not reported")
    config = fake.get_work_group()["WorkGroup"]["Configuration"]
    config["BytesScannedCutoffPerQuery"] = 10 * 1024 * 1024
    config["ResultConfiguration"]["ExpectedBucketOwner"] = "000000000000"
    for cutoff in (None, 10 * 1024 * 1024 + 1):
        changed = dict(config, BytesScannedCutoffPerQuery=cutoff)
        with patch.object(
            fake,
            "get_work_group",
            return_value={"WorkGroup": {"Configuration": changed}},
        ):
            try:
                Probe(
                    fake,
                    fake,
                    "synthetic-workgroup",
                    "000000000000",
                    "s3://synthetic/results/",
                )
            except RuntimeError:
                pass
            else:
                raise RuntimeError("workgroup scan drift was accepted")
    with patch.object(
        fake, "get_work_group", return_value={"WorkGroup": {"Configuration": config}}
    ):
        try:
            Probe(
                fake, fake, "synthetic-workgroup", "000000000000", "s3://other/results/"
            )
        except RuntimeError:
            pass
        else:
            raise RuntimeError("workgroup output drift was accepted")
    expected = "arn:aws:iam::000000000000:role/reader"
    require(
        role_matches(
            {
                "Account": "000000000000",
                "Arn": "arn:aws:sts::000000000000:assumed-role/reader/test",
            },
            expected,
        ),
        "expected role was rejected",
    )
    for wrong in [
        {
            "Account": "000000000000",
            "Arn": "arn:aws:sts::000000000000:assumed-role/admin/test",
        },
        {
            "Account": "111111111111",
            "Arn": "arn:aws:sts::111111111111:assumed-role/reader/test",
        },
    ]:
        require(
            not role_matches(wrong, expected), "unexpected role or account was accepted"
        )
    sample = {
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
    validate_deployment(sample)
    import copy

    for field, invalid in (
        ("bucket", "example-tst-fixtures"),
        ("bucket", "ck-datalake-tst-ckdata-fixtures-111111111111"),
        ("database", "example_tst_fixtures"),
        ("region", "us-east-1"),
        ("table", "other"),
    ):
        changed = copy.deepcopy(sample)
        changed[field] = invalid
        try:
            validate_deployment(changed)
        except RuntimeError:
            pass
        else:
            raise RuntimeError("non-test deployment was accepted")
    admitted = copy.deepcopy(sample)
    admitted["operator_role_arns"] = ["arn:aws:iam::111111111111:role/operator"]
    validate_deployment(admitted)
    for seat, field in (
        ("internal", "workgroup"),
        ("research", "role_arn"),
        ("research", "source_role_arn"),
    ):
        changed = copy.deepcopy(sample)
        changed["seats"][seat][field] += "-near-miss"
        try:
            validate_deployment(changed)
        except RuntimeError:
            pass
        else:
            raise RuntimeError("near-miss test namespace was accepted")
    changed = copy.deepcopy(sample)
    changed["seats"]["research"]["role_arn"] = (
        "arn:aws:iam::111111111111:role/ck-datalake-tst-ckdata-reader"
    )
    try:
        validate_deployment(changed)
    except RuntimeError:
        pass
    else:
        raise RuntimeError("cross-account runtime role was accepted")

    class Denied(Exception):
        def __init__(self):
            super().__init__("synthetic denial")
            self.response = {"Error": {"Code": "AccessDenied"}}

    def denied():
        raise Denied()

    expect_denied(denied, "offline denial classifier")
    body = io.BytesIO(b"synthetic")
    try:
        expect_denied(lambda: {"Body": body}, "offline unexpected success")
    except RuntimeError:
        require(body.closed, "unexpected successful GET body was not closed")
    else:
        raise RuntimeError("unexpected access was accepted")
    print("PASS OFFLINE synthetic checks; no AWS clients were constructed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--run-live", action="store_true")
    parser.add_argument("--deployment", type=Path)
    args = parser.parse_args()
    if args.self_test:
        require(
            not args.run_live and args.deployment is None,
            "self-test cannot be combined with live arguments",
        )
        self_test()
        return
    require(
        args.run_live and args.deployment is not None,
        "live mode requires --run-live and --deployment",
    )
    with args.deployment.open("rb") as contract:
        raw = contract.read(65536 + 1)
    require(len(raw) <= 65536, "deployment contract is unexpectedly large")
    run_live(json.loads(raw))


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("STOP interrupted", file=sys.stderr)
        sys.exit(130)
    except Exception as error:  # noqa: BLE001 -- CLI boundary logs only a code or safe local error
        sys.stdout.flush()
        code = getattr(error, "response", {}).get("Error", {}).get("Code")
        if code:
            print(
                f"FAIL {getattr(error, 'operation_name', 'AWS')}: {code}",
                file=sys.stderr,
            )
        elif isinstance(error, (RuntimeError, TimeoutError)):
            print(f"FAIL {error}", file=sys.stderr)
        else:
            print(
                f"FAIL {type(error).__name__}; no response body logged", file=sys.stderr
            )
        sys.exit(1)
