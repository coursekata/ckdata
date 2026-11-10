//! Offline fixtures for tests of the installed R library. These private native
//! entrypoints stay compiled so testthat can exercise the real cursors and jobs.
use super::*;

use arrow_array::types::TimestampMillisecondType;
use arrow_array::{ArrayRef, Int32Array, Int64Array, PrimitiveArray, StringArray};

fn fixture_reader() -> std::result::Result<impl RecordBatchReader + Send, arrow_schema::ArrowError>
{
    let columns: Vec<(&str, ArrayRef)> = vec![
        ("id", Arc::new(Int32Array::from(vec![1, 2]))),
        ("s_null", Arc::new(StringArray::from(vec![None, Some("a")]))),
        (
            "s_empty",
            Arc::new(StringArray::from(vec![Some(""), Some("b")])),
        ),
        (
            "big",
            Arc::new(Int64Array::from(vec![Some(9_007_199_254_740_993), None])),
        ),
        (
            "ts",
            Arc::new(PrimitiveArray::<TimestampMillisecondType>::from(vec![
                Some(1_792_154_096_789),
                None,
            ])),
        ),
    ];
    let batch = RecordBatch::try_from_iter(columns)?;
    let schema = batch.schema();
    Ok(RecordBatchIterator::new(std::iter::once(Ok(batch)), schema))
}

/// Native result fixture. Entirely offline, with the same cursor as AWS results.
/// @noRd
#[extendr]
fn native_probe_result(empty: bool) -> Result<Robj> {
    let mut result =
        NativeResult::from_reader(fixture_reader().map_err(failure)?).map_err(failure)?;
    if empty {
        result.batches.clear();
        result.rows = 0;
    }
    Ok(ExternalPtr::new(result).into())
}

/// Offline workgroup fixture using the same R conversion as live metadata.
/// @noRd
#[extendr]
fn native_probe_workgroup() -> List {
    WorkgroupInfo {
        output_location: Some(
            "s3://ck-datalake-tst-ckdata-fixtures-000000000000/results/internal/".to_owned(),
        ),
        expected_bucket_owner: Some("000000000000".to_owned()),
        encryption_option: Some("SSE_S3".to_owned()),
        kms_key: None,
        enforce_work_group_configuration: Some(true),
        bytes_scanned_cutoff_per_query: Some(10 * 1024 * 1024),
        selected_engine_version: Some("Athena engine version 3".to_owned()),
    }
    .into_list()
}

/// Offline table fixture, including a partition key, through live metadata conversion.
/// @noRd
#[extendr]
fn native_probe_metadata() -> List {
    let column = |name: &str, data_type: &str| ckdata::ColumnMetadata {
        name: name.to_owned(),
        data_type: Some(data_type.to_owned()),
        comment: None,
    };
    columns(ckdata::TableMetadata {
        name: "types".to_owned(),
        table_type: None,
        columns: vec![
            column("id", "integer"),
            column("s_null", "varchar"),
            column("s_empty", "varchar"),
            column("big", "bigint"),
        ],
        partition_keys: vec![column("ts", "timestamp")],
    })
}

/// Offline asynchronous fixture. Pending jobs complete only on cancellation.
/// @noRd
#[extendr]
fn native_probe_job(pending: bool) -> Result<Robj> {
    let cancellation = QueryCancellation::new();
    let cancel = cancellation.clone();
    spawn(cancellation, false, async move {
        if pending {
            cancel.cancelled().await;
            return Err(NativeFailure::message("synthetic query cancelled"));
        }
        NativeResult::from_reader(fixture_reader().map_err(NativeFailure::message)?)
            .map(Completion::Probe)
            .map_err(NativeFailure::message)
    })
}

/// Offline sign-in metadata through the same conversion as pending jobs and errors.
/// @noRd
#[extendr]
fn native_probe_login_targets() -> List {
    let legacy = ckdata::SsoLoginTarget {
        profile: "synthetic".into(),
        session: None,
        config: None,
    };
    let profile = ckdata::SsoLoginTarget {
        session: Some("synthetic-session".into()),
        ..legacy.clone()
    };
    let direct = ckdata::SsoLoginTarget {
        config: Some("[profile synthetic]\nsso_session = synthetic-session\n[sso-session synthetic-session]\nsso_region = us-west-1\nsso_start_url = https://synthetic.awsapps.com/start\n".into()),
        ..profile.clone()
    };
    list!(
        legacy = login_target_list(legacy.clone()),
        profile = login_target_list(profile),
        direct = login_target_list(direct),
        failure =
            NativeFailure::from(ckdata::Error::SsoLoginRequired { target: legacy }).into_list()
    )
}

extendr_module! {
    mod test_support;
    fn native_probe_result;
    fn native_probe_workgroup;
    fn native_probe_metadata;
    fn native_probe_job;
    fn native_probe_login_targets;
}
