"""Synthetic deployment and identity fences, checked before any row query."""

import re


def validate_deployment(deployment, seat_name):
    if not isinstance(deployment, dict) or seat_name not in ("internal", "research"):
        raise ValueError("select an internal or research synthetic deployment seat")
    account = deployment.get("account_id")
    if not isinstance(account, str) or not re.fullmatch(r"[0-9]{12}", account):
        raise ValueError("synthetic account_id must contain 12 digits")
    expected = {
        "purpose": "ckdata-synthetic-tests",
        "format_version": 1,
        "region": "us-west-2",
        "database": "ck_datalake_tst_ckdata_fixtures",
        "table": "types",
        "bucket": f"ck-datalake-tst-ckdata-fixtures-{account}",
    }
    for key, value in expected.items():
        if type(deployment.get(key)) is not type(value) or deployment[key] != value:
            raise ValueError(f"{key} does not match the synthetic fixture contract")
    seat = deployment.get("seats", {}).get(seat_name)
    expected_seat = {
        "role_arn": f"arn:aws:iam::{account}:role/ck-datalake-tst-ckdata-{seat_name}-reader",
        "workgroup": f"ck-datalake-tst-ckdata-{seat_name}",
        "output_location": f"s3://{deployment['bucket']}/results/{seat_name}/",
    }
    if not isinstance(seat, dict) or any(
        seat.get(key) != value for key, value in expected_seat.items()
    ):
        raise ValueError("seat does not match the restricted synthetic reader contract")
    return seat


def validate_identity(info, deployment, seat_name):
    seat = validate_deployment(deployment, seat_name)
    expected = {
        "account": deployment["account_id"],
        "region": deployment["region"],
        "schema": deployment["database"],
        "workgroup": seat["workgroup"],
    }
    prefix = f"arn:aws:sts::{deployment['account_id']}:assumed-role/ck-datalake-tst-ckdata-{seat_name}-reader/"
    if any(info.get(key) != value for key, value in expected.items()) or not info.get(
        "arn", ""
    ).startswith(prefix):
        raise ValueError(
            "connection must use the selected restricted synthetic reader before querying"
        )


def validate_workgroup(info, deployment, seat):
    expected = {
        "output_location": seat["output_location"],
        "expected_bucket_owner": deployment["account_id"],
        "encryption_option": "SSE_S3",
        "kms_key": None,
        "enforce_work_group_configuration": True,
        "bytes_scanned_cutoff_per_query": 10 * 1024 * 1024,
        "selected_engine_version": "Athena engine version 3",
    }
    if any(
        type(info.get(key)) is not type(value) or info[key] != value
        for key, value in expected.items()
    ):
        raise ValueError(
            "workgroup must match the encrypted synthetic result-path contract"
        )
