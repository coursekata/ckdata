"""Acceptance cannot reach row queries without the exact synthetic contract."""

from copy import deepcopy

import pytest

from .acceptance import validate_deployment, validate_identity, validate_workgroup


@pytest.fixture
def deployment():
    account = "111111111111"
    return {
        "purpose": "ckdata-synthetic-tests",
        "format_version": 1,
        "account_id": account,
        "region": "us-west-2",
        "database": "ck_datalake_tst_ckdata_fixtures",
        "table": "types",
        "bucket": f"ck-datalake-tst-ckdata-fixtures-{account}",
        "seats": {
            "research": {
                "role_arn": f"arn:aws:iam::{account}:role/ck-datalake-tst-ckdata-research-reader",
                "workgroup": "ck-datalake-tst-ckdata-research",
                "output_location": f"s3://ck-datalake-tst-ckdata-fixtures-{account}/results/research/",
            }
        },
    }


@pytest.mark.parametrize(
    "field,value",
    [
        ("purpose", "other"),
        ("format_version", True),
        ("database", "real_data"),
        ("table", "other_table"),
        ("region", "us-east-1"),
        ("account_id", "invalid"),
    ],
)
def test_non_synthetic_deployment_is_rejected(deployment, field, value):
    deployment[field] = value
    with pytest.raises(ValueError):
        validate_deployment(deployment, "research")


def test_identity_and_workgroup_fences_reject_ambient_role_or_unencrypted_results(
    deployment,
):
    seat = validate_deployment(deployment, "research")
    info = {
        "account": deployment["account_id"],
        "region": "us-west-2",
        "schema": deployment["database"],
        "workgroup": seat["workgroup"],
        "arn": "arn:aws:sts::111111111111:assumed-role/ck-datalake-tst-ckdata-research-reader/test",
    }
    validate_identity(info, deployment, "research")
    with pytest.raises(ValueError):
        validate_identity(
            info | {"arn": "arn:aws:sts::111111111111:assumed-role/ambient/test"},
            deployment,
            "research",
        )
    workgroup = {
        "output_location": seat["output_location"],
        "expected_bucket_owner": deployment["account_id"],
        "encryption_option": "SSE_S3",
        "kms_key": None,
        "enforce_work_group_configuration": True,
        "bytes_scanned_cutoff_per_query": 10 * 1024 * 1024,
        "selected_engine_version": "Athena engine version 3",
    }
    validate_workgroup(workgroup, deployment, seat)
    with pytest.raises(ValueError):
        validate_workgroup(workgroup | {"encryption_option": "NONE"}, deployment, seat)
    altered = deepcopy(deployment)
    altered["seats"]["research"]["role_arn"] = "arn:aws:iam::111111111111:role/ambient"
    with pytest.raises(ValueError):
        validate_deployment(altered, "research")
