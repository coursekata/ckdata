//! Fixed, synthetic-value acceptance checks. No arbitrary SQL, cleanup or IAM writes.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use arrow_array::types::{
    TimestampMicrosecondType, TimestampMillisecondType, TimestampNanosecondType,
    TimestampSecondType,
};
use arrow_array::{
    Array, Decimal128Array, Int32Array, Int64Array, ListArray, PrimitiveArray, StringArray,
};
use aws_config::BehaviorVersion;
use aws_credential_types::{Credentials, provider::SharedCredentialsProvider};
use aws_types::{SdkConfig, region::Region};
use ckdata::{
    CallerIdentity, Connection, ConnectionOptions, Error, QueryCancellation, QueryInterruption,
    QueryOptions, QueryResult, SessionStatus,
};
use clap::Parser;
use serde::Deserialize;

const PROBE: &str = "SELECT * FROM (VALUES (1, CAST(NULL AS VARCHAR), '', BIGINT '9007199254740993', DECIMAL '1.10', TIMESTAMP '2026-10-16 12:34:56.789', ARRAY[1, 2]), (2, 'a', 'b', NULL, NULL, NULL, NULL)) AS t(id, s_null, s_empty, big, dec, ts, arr)";

#[derive(Debug, Parser)]
#[command(
    about = "Check the Rust Athena/Arrow path using fixed invented values. No data is printed or deleted."
)]
struct Args {
    /// Required to construct AWS clients or make any live request.
    #[arg(long)]
    run_live: bool,
    /// Synthetic fixture contract from ck-datalake. Runs both restricted seats.
    #[arg(long, conflicts_with_all = ["workgroup", "schema", "role_arn", "expected_account", "expected_role", "table"])]
    deployment: Option<PathBuf>,
    /// Standard AWS source profile (SSO login is handled by the AWS CLI).
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    region: Option<String>,
    /// Controlled final acceptance only; fixed literal SQL, never a table query.
    #[arg(long, required_unless_present = "deployment")]
    workgroup: Option<String>,
    #[arg(long, required_unless_present = "deployment")]
    schema: Option<String>,
    /// An optional additional role, assumed once by the Rust connection.
    #[arg(long)]
    role_arn: Option<String>,
    #[arg(long, required_unless_present = "deployment")]
    expected_account: Option<String>,
    /// Exact IAM role ARN for the selected identity, checked before data requests.
    #[arg(long, required_unless_present = "deployment")]
    expected_role: Option<String>,
    /// Optional catalog-only metadata check; never selects rows from this table.
    #[arg(long)]
    table: Option<String>,
    /// Wait for actual role expiry, then verify a new query fails before submission.
    #[arg(long)]
    wait_for_expiry: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Deployment {
    format_version: u32,
    purpose: String,
    account_id: String,
    bucket: String,
    database: String,
    fixture_keys: Vec<String>,
    operator_role_arns: Vec<String>,
    region: String,
    seats: BTreeMap<String, Seat>,
    table: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Seat {
    output_location: String,
    role_arn: String,
    source_role_arn: Option<String>,
    workgroup: String,
}

fn account_valid(account: &str) -> bool {
    account.len() == 12 && account.bytes().all(|ch| ch.is_ascii_digit())
}

fn validate(deployment: &Deployment, region: Option<&str>) -> Result<(), String> {
    let account = &deployment.account_id;
    require(
        deployment.format_version == 1 && deployment.purpose == "ckdata-synthetic-tests",
        "not a supported synthetic deployment contract",
    )?;
    require(
        account_valid(account)
            && deployment.bucket == format!("ck-datalake-tst-ckdata-fixtures-{account}"),
        "unexpected synthetic account or bucket",
    )?;
    require(
        deployment.database == "ck_datalake_tst_ckdata_fixtures" && deployment.table == "types",
        "unexpected synthetic database or table",
    )?;
    require(
        deployment.fixture_keys
            == [
                "fixtures/types/part-1.parquet",
                "fixtures/types/part-2.parquet",
            ],
        "unexpected fixture keys",
    )?;
    require(
        !deployment.region.is_empty() && region.is_none_or(|region| region == deployment.region),
        "region differs from the deployment contract",
    )?;
    require(
        deployment
            .seats
            .keys()
            .map(String::as_str)
            .eq(["internal", "research"]),
        "expected the two synthetic seats",
    )?;
    require(
        !deployment.operator_role_arns.is_empty()
            && deployment.operator_role_arns.iter().all(|arn| {
                arn.starts_with(&format!("arn:aws:iam::{account}:role/")) && !arn.contains('*')
            }),
        "invalid operator role fence",
    )?;
    for (name, seat) in &deployment.seats {
        require(
            seat.workgroup == format!("ck-datalake-tst-ckdata-{name}")
                && seat.role_arn
                    == format!("arn:aws:iam::{account}:role/ck-datalake-tst-ckdata-{name}-reader")
                && seat.output_location == format!("s3://{}/results/{name}/", deployment.bucket),
            "synthetic seat escaped its namespace",
        )?;
        let expected_source = if name == "research" {
            Some(format!(
                "arn:aws:iam::{account}:role/ck-datalake-tst-ckdata-source"
            ))
        } else {
            None
        };
        require(
            seat.source_role_arn == expected_source,
            "unexpected synthetic source role",
        )?;
    }
    Ok(())
}

fn require(condition: bool, reason: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(reason.to_owned())
    }
}

fn identity_matches(identity: &CallerIdentity, account: &str, role: &str) -> bool {
    let prefix = format!("arn:aws:iam::{account}:role/");
    let Some(role_name) = role
        .strip_prefix(&prefix)
        .and_then(|name| name.rsplit('/').next())
    else {
        return false;
    };
    identity.account == account
        && identity
            .arn
            .starts_with(&format!("arn:aws:sts::{account}:assumed-role/{role_name}/"))
}

fn safe_core_error(error: Error) -> String {
    match error {
        Error::QueryFailed { query_id, .. } => {
            format!("Athena failed; query_id={query_id}; inspect its reason in Athena")
        }
        error => error.to_string(),
    }
}

async fn source_role(config: SdkConfig, role: &str) -> Result<SdkConfig, String> {
    let response = aws_sdk_sts::Client::new(&config)
        .assume_role()
        .role_arn(role)
        .role_session_name("ckdata-acceptance-source")
        .duration_seconds(3600)
        .send()
        .await
        .map_err(|_| "cannot assume the synthetic source role".to_owned())?;
    let credentials = response
        .credentials()
        .ok_or("source role returned no credentials")?;
    let expires = SystemTime::try_from(*credentials.expiration())
        .map_err(|_| "source role returned invalid expiry")?;
    Ok(config
        .to_builder()
        .credentials_provider(SharedCredentialsProvider::new(Credentials::new(
            credentials.access_key_id(),
            credentials.secret_access_key(),
            Some(credentials.session_token().to_owned()),
            Some(expires),
            "synthetic-source",
        )))
        .build())
}

async fn run_query(connection: &Connection, sql: &str) -> Result<QueryResult, String> {
    let token = QueryCancellation::new();
    let options = QueryOptions::default();
    let future = connection.query(sql, &options, &token);
    tokio::pin!(future);
    tokio::select! {
        result = &mut future => result.map_err(safe_core_error),
        signal = tokio::signal::ctrl_c() => {
            signal.map_err(|_| "cannot listen for Ctrl-C")?;
            token.cancel();
            future.await.map_err(safe_core_error)
        }
    }
}

fn check_types(result: &QueryResult) -> Result<(), String> {
    require(
        result.row_count() == 2 && result.warnings().is_empty(),
        "literal count or ordering warning changed",
    )?;
    let batch = result
        .batches()
        .first()
        .ok_or("literal result has no batches")?;
    require(
        result.batches().len() == 1 && batch.num_rows() == 2,
        "ordered probe was not combined",
    )?;
    let id = batch
        .column(0)
        .as_any()
        .downcast_ref::<Int32Array>()
        .ok_or("id type changed")?;
    let nulls = batch
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or("string type changed")?;
    let empty = batch
        .column(2)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or("empty-string type changed")?;
    let big = batch
        .column(3)
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or("bigint type changed")?;
    let dec = batch
        .column(4)
        .as_any()
        .downcast_ref::<Decimal128Array>()
        .ok_or("decimal type changed")?;
    let timestamp = batch.column(5);
    let timestamp_ns = match timestamp.data_type() {
        arrow_schema::DataType::Timestamp(unit, None) => match unit {
            arrow_schema::TimeUnit::Second => timestamp
                .as_any()
                .downcast_ref::<PrimitiveArray<TimestampSecondType>>()
                .map(|array| array.value(1) as i128 * 1_000_000_000),
            arrow_schema::TimeUnit::Millisecond => timestamp
                .as_any()
                .downcast_ref::<PrimitiveArray<TimestampMillisecondType>>()
                .map(|array| array.value(1) as i128 * 1_000_000),
            arrow_schema::TimeUnit::Microsecond => timestamp
                .as_any()
                .downcast_ref::<PrimitiveArray<TimestampMicrosecondType>>()
                .map(|array| array.value(1) as i128 * 1_000),
            arrow_schema::TimeUnit::Nanosecond => timestamp
                .as_any()
                .downcast_ref::<PrimitiveArray<TimestampNanosecondType>>()
                .map(|array| array.value(1) as i128),
        },
        _ => None,
    }
    .ok_or("timestamp type changed")?;
    let arr = batch
        .column(6)
        .as_any()
        .downcast_ref::<ListArray>()
        .ok_or("array type changed")?;
    require(
        id.values().as_ref() == [2, 1]
            && nulls.value(0) == "a"
            && nulls.is_null(1)
            && empty.value(0) == "b"
            && !empty.is_null(1)
            && empty.value(1).is_empty()
            && big.is_null(0)
            && big.value(1) == 9_007_199_254_740_993
            && dec.is_null(0)
            && dec.value(1) == 110
            && dec.scale() == 2
            && timestamp.is_null(0)
            && !timestamp.is_null(1)
            && timestamp_ns == 1_792_154_096_789_000_000
            && arr.is_null(0)
            && arr.value_length(1) == 2,
        "literal values changed",
    )?;
    let values = arr.value(1);
    let values = values
        .as_any()
        .downcast_ref::<Int32Array>()
        .ok_or("array element type changed")?;
    require(values.values().as_ref() == [1, 2], "array values changed")
}

async fn checks(
    connection: &Connection,
    table: Option<&str>,
    synthetic_table: bool,
) -> Result<(), String> {
    let sql = format!("{PROBE} ORDER BY id DESC");
    let result = run_query(connection, &sql).await?;
    check_types(&result)?;
    println!("PASS literal Arrow types, values and final ordering");
    let empty = run_query(
        connection,
        &format!("SELECT * FROM ({PROBE}) q WHERE FALSE ORDER BY id"),
    )
    .await?;
    require(
        empty.row_count() == 0 && empty.schema() == result.schema(),
        "empty result lost its exact schema",
    )?;
    println!("PASS empty result retains schema");
    let all_null = run_query(connection, "SELECT CAST(NULL AS BIGINT) AS all_null").await?;
    require(
        all_null.row_count() == 1 && all_null.batches()[0].column(0).is_null(0),
        "all-NULL real row was discarded",
    )?;
    println!("PASS all-NULL real row retained");
    if let Some(table) = table {
        let metadata = connection
            .table_metadata(table)
            .await
            .map_err(safe_core_error)?;
        require(!metadata.columns.is_empty(), "table has no catalog columns")?;
        println!("PASS table and column metadata without query submission");
        if synthetic_table {
            let table = format!("\"{}\"", table.replace('"', "\"\""));
            let result = run_query(
                connection,
                &format!(
                    "SELECT id, s_null, s_empty, big, dec, ts, arr FROM {table} ORDER BY id DESC"
                ),
            )
            .await?;
            check_types(&result)?;
            println!("PASS invented fixture table through Rust reader");
        }
    }
    Ok(())
}

async fn run(args: Args) -> Result<(), String> {
    // Validate all offline inputs before resolving credentials or making clients.
    let deployment = args
        .deployment
        .as_ref()
        .map(|path| {
            let bytes = std::fs::read(path).map_err(|_| "cannot read deployment contract")?;
            require(bytes.len() <= 65536, "deployment contract is too large")?;
            let deployment: Deployment =
                serde_json::from_slice(&bytes).map_err(|_| "invalid deployment contract")?;
            validate(&deployment, args.region.as_deref())?;
            Ok::<_, String>(deployment)
        })
        .transpose()?;
    require(
        args.run_live,
        "no AWS clients created; add --run-live only when ready for live acceptance",
    )?;
    if args.wait_for_expiry {
        require(
            deployment.is_some() || args.role_arn.is_some(),
            "expiry check requires an explicit additional role",
        )?;
    }
    if deployment.is_none() {
        require(
            account_valid(args.expected_account.as_deref().unwrap_or_default()),
            "expected account must have twelve digits",
        )?;
    }
    let mut loader = aws_config::defaults(BehaviorVersion::latest());
    if let Some(profile) = args.profile {
        loader = loader.profile_name(profile);
    }
    if let Some(region) = deployment
        .as_ref()
        .map(|deployment| deployment.region.clone())
        .or(args.region)
    {
        loader = loader.region(Region::new(region));
    }
    let base = loader.load().await;
    let mut connections = Vec::new();
    if let Some(deployment) = deployment {
        let operator = Connection::connect_with_config(
            ConnectionOptions::new("synthetic-identity-check", &deployment.database),
            base.clone(),
        )
        .await
        .map_err(safe_core_error)?;
        let identity = operator.caller_identity().await.map_err(safe_core_error)?;
        require(
            deployment
                .operator_role_arns
                .iter()
                .any(|role| identity_matches(&identity, &deployment.account_id, role)),
            "operator identity is not admitted by the synthetic deployment",
        )?;
        for (name, seat) in deployment.seats {
            let mut config = base.clone();
            if let Some(source) = &seat.source_role_arn {
                config = source_role(config, source).await?;
                let source_connection = Connection::connect_with_config(
                    ConnectionOptions::new("synthetic-identity-check", &deployment.database),
                    config.clone(),
                )
                .await
                .map_err(safe_core_error)?;
                require(
                    identity_matches(
                        &source_connection
                            .caller_identity()
                            .await
                            .map_err(safe_core_error)?,
                        &deployment.account_id,
                        source,
                    ),
                    "synthetic source identity differs",
                )?;
            }
            let mut options = ConnectionOptions::new(seat.workgroup, &deployment.database);
            options.role_arn = Some(seat.role_arn.clone());
            let connection = Connection::connect_with_config(options, config)
                .await
                .map_err(safe_core_error)?;
            require(
                identity_matches(
                    &connection
                        .caller_identity()
                        .await
                        .map_err(safe_core_error)?,
                    &deployment.account_id,
                    &seat.role_arn,
                ),
                "synthetic reader identity differs",
            )?;
            println!("PASS {name}: expected synthetic account and reader");
            checks(&connection, Some(&deployment.table), true).await?;
            connections.push(connection);
        }
    } else {
        let mut options = ConnectionOptions::new(
            args.workgroup.ok_or("missing workgroup")?,
            args.schema.ok_or("missing schema")?,
        );
        options.role_arn = args.role_arn;
        let connection = Connection::connect_with_config(options, base)
            .await
            .map_err(safe_core_error)?;
        require(
            identity_matches(
                &connection
                    .caller_identity()
                    .await
                    .map_err(safe_core_error)?,
                args.expected_account
                    .as_deref()
                    .ok_or("missing expected account")?,
                args.expected_role
                    .as_deref()
                    .ok_or("missing expected role")?,
            ),
            "final-acceptance identity differs; no data query submitted",
        )?;
        checks(&connection, args.table.as_deref(), false).await?;
        connections.push(connection);
    }
    if args.wait_for_expiry {
        let remaining = connections
            .iter()
            .map(|connection| match connection.session_status() {
                Ok(
                    SessionStatus::Assumed { remaining, .. }
                    | SessionStatus::Expiring { remaining, .. },
                ) => Ok(remaining),
                _ => Err("connection has no active explicit-role lease"),
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .max()
            .unwrap_or_default();
        println!(
            "INFO waiting for actual one-hour role expiry; no renewal or queries during the wait"
        );
        tokio::time::sleep(remaining + Duration::from_secs(1)).await;
        for connection in connections {
            require(
                matches!(
                    connection
                        .query(
                            "SELECT 1",
                            &QueryOptions::default(),
                            &QueryCancellation::new()
                        )
                        .await,
                    Err(Error::QueryInterrupted {
                        query_id: None,
                        reason: QueryInterruption::SessionExpired,
                        ..
                    })
                ),
                "expired role did not reject a new query before submission",
            )?;
        }
        println!("PASS actual assumed-role expiry rejects new queries");
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    match run(Args::parse()).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("FAIL {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn help_and_live_gate_require_no_aws_configuration() {
        assert!(Args::try_parse_from(["probe", "--help"]).is_err());
        let args = Args::try_parse_from([
            "probe",
            "--workgroup",
            "fake",
            "--schema",
            "fake",
            "--expected-account",
            "000000000000",
            "--expected-role",
            "arn:aws:iam::000000000000:role/fake",
        ])
        .unwrap();
        assert!(!args.run_live);
        assert!(
            Args::try_parse_from(["probe", "--deployment", "fake.json", "--workgroup", "other"])
                .is_err()
        );
    }
    #[test]
    fn identity_fence_matches_exact_role_name_and_account() {
        let identity = CallerIdentity {
            account: "000000000000".into(),
            arn: "arn:aws:sts::000000000000:assumed-role/test-reader/session".into(),
        };
        assert!(identity_matches(
            &identity,
            "000000000000",
            "arn:aws:iam::000000000000:role/test-reader"
        ));
        assert!(!identity_matches(
            &identity,
            "000000000000",
            "arn:aws:iam::000000000000:role/test-reader-extra"
        ));
        assert!(!identity_matches(
            &identity,
            "111111111111",
            "arn:aws:iam::111111111111:role/test-reader"
        ));
    }
}
