//! Fixed, synthetic-value acceptance checks. No arbitrary SQL, cleanup or IAM writes.
use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use arrow_array::types::{
    TimestampMicrosecondType, TimestampMillisecondType, TimestampNanosecondType,
    TimestampSecondType,
};
use arrow_array::{
    Array, Decimal128Array, Int32Array, Int64Array, ListArray, PrimitiveArray, RecordBatchReader,
    StringArray,
};
use aws_config::BehaviorVersion;
use aws_credential_types::{Credentials, provider::SharedCredentialsProvider};
use aws_sdk_athena::types::{EncryptionOption, WorkGroupConfiguration};
use aws_types::{SdkConfig, region::Region};
use ckdata::{
    CallerIdentity, Connection, ConnectionOptions, Error, QueryCancellation, QueryInterruption,
    QueryOptions, QueryResult, SessionStatus,
};
use clap::Parser;
use serde::Deserialize;

const PROBE: &str = "SELECT * FROM (VALUES (1, CAST(NULL AS VARCHAR), '', BIGINT '9007199254740993', DECIMAL '1.10', TIMESTAMP '2026-10-16 12:34:56.789', ARRAY[1, 2]), (2, 'a', 'b', NULL, NULL, NULL, NULL)) AS t(id, s_null, s_empty, big, dec, ts, arr)";

const SCALAR_PROBE: &str = r#"SELECT
TRUE AS flag,
CAST(7 AS TINYINT) AS tiny,
CAST(32000 AS SMALLINT) AS small,
42 AS integer_value,
BIGINT '9007199254740993' AS big,
REAL '1.25' AS single_value,
DOUBLE '1.25' AS double_value,
DECIMAL '1.10' AS decimal_small,
CAST('12345678901234567890.123456789012345678' AS DECIMAL(38,18)) AS decimal_wide,
CAST('invented' AS VARCHAR) AS text_value,
CAST('xy' AS CHAR(5)) AS fixed_value,
CAST('' AS VARCHAR) AS empty_text,
CAST(NULL AS VARCHAR) AS null_text,
X'00017FFF' AS bytes_value,
DATE '2026-10-16' AS day_value,
TIMESTAMP '2026-10-16 12:34:56.789' AS moment,
TIMESTAMP '1960-01-02 03:04:05.678' AS before_epoch"#;

const NESTED_PROBE: &str = r#"SELECT
ARRAY[BIGINT '9007199254740993', CAST(NULL AS BIGINT), BIGINT '-4'] AS array_nullable,
CAST(ARRAY[] AS ARRAY(INTEGER)) AS empty_array,
ARRAY[CAST('1.123456789012345678' AS DECIMAL(38,18)), CAST(NULL AS DECIMAL(38,18))] AS decimal_array,
MAP(ARRAY['a','b'], ARRAY[CAST('1.10' AS DECIMAL(38,18)), CAST(NULL AS DECIMAL(38,18))]) AS map_decimal,
CAST(ROW(BIGINT '9007199254740993','invented',ARRAY[1,CAST(NULL AS INTEGER),2]) AS ROW(id BIGINT,label VARCHAR,items ARRAY(INTEGER))) AS named_record,
CAST(ARRAY[ROW(BIGINT '3',ARRAY[1,CAST(NULL AS INTEGER)]),CAST(NULL AS ROW(id BIGINT,items ARRAY(INTEGER)))] AS ARRAY(ROW(id BIGINT,items ARRAY(INTEGER)))) AS record_array,
CAST(MAP(ARRAY['a','b'],ARRAY[ARRAY[ROW(DECIMAL '1.10',TIMESTAMP '2026-10-16 12:34:56.789')],CAST(NULL AS ARRAY(ROW(amount DECIMAL(10,2),stamp TIMESTAMP)))]) AS MAP(VARCHAR,ARRAY(ROW(amount DECIMAL(10,2),stamp TIMESTAMP)))) AS map_records,
CAST(ROW(ARRAY[ARRAY[1,CAST(NULL AS INTEGER)],CAST(NULL AS ARRAY(INTEGER)),CAST(ARRAY[] AS ARRAY(INTEGER))],MAP(ARRAY['a'],ARRAY[ARRAY[BIGINT '9007199254740993',CAST(NULL AS BIGINT)]])) AS ROW(matrix ARRAY(ARRAY(INTEGER)),lookup MAP(VARCHAR,ARRAY(BIGINT)))) AS deep_record,
CAST(NULL AS ROW(id BIGINT,items ARRAY(INTEGER))) AS null_record"#;

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
        deployment.region == "us-west-2" && region.is_none_or(|region| region == deployment.region),
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
            && deployment
                .operator_role_arns
                .iter()
                .all(|arn| role_account(arn).is_some()),
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
    error.to_string()
}

fn role_account(arn: &str) -> Option<&str> {
    let (account, name) = arn.strip_prefix("arn:aws:iam::")?.split_once(":role/")?;
    (account_valid(account)
        && !name.is_empty()
        && name
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || b"+=,.@_/-".contains(&ch)))
    .then_some(account)
}

fn operator_matches(identity: &CallerIdentity, role: &str) -> bool {
    role_account(role).is_some_and(|account| identity_matches(identity, account, role))
}

fn validate_workgroup(
    config: &WorkGroupConfiguration,
    seat: &Seat,
    account: &str,
) -> Result<(), String> {
    require(
        config.enforce_work_group_configuration() == Some(true),
        "workgroup controls are not enforced",
    )?;
    require(
        config.bytes_scanned_cutoff_per_query() == Some(10 * 1024 * 1024),
        "workgroup scan cutoff differs from the 10 MiB fixture contract",
    )?;
    require(
        config
            .engine_version()
            .and_then(|engine| engine.selected_engine_version())
            == Some("Athena engine version 3"),
        "unexpected Athena engine",
    )?;
    let result = config
        .result_configuration()
        .ok_or("workgroup has no result configuration")?;
    require(
        result.output_location() == Some(seat.output_location.as_str()),
        "workgroup output differs from the synthetic deployment",
    )?;
    require(
        result.expected_bucket_owner() == Some(account)
            && result.encryption_configuration().is_some_and(|encryption| {
                encryption.encryption_option() == &EncryptionOption::SseS3
                    && encryption.kms_key().is_none()
            }),
        "workgroup owner or encryption differs from the fixture contract",
    )
}

fn read_contract(reader: impl Read) -> Result<Deployment, String> {
    let mut bytes = Vec::new();
    reader
        .take(65536 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read deployment contract")?;
    require(bytes.len() <= 65536, "deployment contract is too large")?;
    serde_json::from_slice(&bytes).map_err(|_| "invalid deployment contract".to_owned())
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
        "literal count or session warning changed",
    )?;
    let schema = result.schema();
    check_probe_batches(&schema, result.batches())
}

fn check_probe_batches(
    schema: &arrow_schema::SchemaRef,
    batches: &[arrow_array::RecordBatch],
) -> Result<(), String> {
    // The reader may retain several batches even for this two-row probe.
    let batch = arrow_select::concat::concat_batches(schema, batches)
        .map_err(|_| "cannot combine the fixed probe")?;
    require(
        batch.num_rows() == 2 && batch.num_columns() == 7,
        "literal shape changed",
    )?;
    let id = batch
        .column(0)
        .as_any()
        .downcast_ref::<Int32Array>()
        .ok_or("id type changed")?;
    let (first, second) = match id.iter().collect::<Vec<_>>().as_slice() {
        [Some(1), Some(2)] => (0, 1),
        [Some(2), Some(1)] => (1, 0),
        _ => return Err("literal ids changed".to_owned()),
    };
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
                .map(|array| array.value(first) as i128 * 1_000_000_000),
            arrow_schema::TimeUnit::Millisecond => timestamp
                .as_any()
                .downcast_ref::<PrimitiveArray<TimestampMillisecondType>>()
                .map(|array| array.value(first) as i128 * 1_000_000),
            arrow_schema::TimeUnit::Microsecond => timestamp
                .as_any()
                .downcast_ref::<PrimitiveArray<TimestampMicrosecondType>>()
                .map(|array| array.value(first) as i128 * 1_000),
            arrow_schema::TimeUnit::Nanosecond => timestamp
                .as_any()
                .downcast_ref::<PrimitiveArray<TimestampNanosecondType>>()
                .map(|array| array.value(first) as i128),
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
        !nulls.is_null(second)
            && nulls.value(second) == "a"
            && nulls.is_null(first)
            && !empty.is_null(second)
            && empty.value(second) == "b"
            && !empty.is_null(first)
            && empty.value(first).is_empty()
            && big.is_null(second)
            && !big.is_null(first)
            && big.value(first) == 9_007_199_254_740_993
            && dec.is_null(second)
            && !dec.is_null(first)
            && dec.precision() == 3
            && dec.value(first) == 110
            && dec.scale() == 2
            && timestamp.is_null(second)
            && !timestamp.is_null(first)
            && timestamp_ns == 1_792_154_096_789_000_000
            && arr.is_null(second)
            && !arr.is_null(first)
            && arr.value_length(first) == 2,
        "literal values changed",
    )?;
    let values = arr.value(first);
    let values = values
        .as_any()
        .downcast_ref::<Int32Array>()
        .ok_or("array element type changed")?;
    require(values.iter().eq([Some(1), Some(2)]), "array values changed")
}

async fn checks(
    connection: &Connection,
    table: Option<&str>,
    synthetic_table: bool,
) -> Result<(), String> {
    let sql = format!("{PROBE} ORDER BY id DESC");
    let result = run_query(connection, &sql).await?;
    check_types(&result)?;
    println!("PASS literal Arrow types and values");
    check_empty_result(
        run_query(
            connection,
            &format!("SELECT * FROM ({PROBE}) q WHERE FALSE ORDER BY id"),
        )
        .await?,
        &result.schema(),
    )?;
    println!("PASS empty result: zero rows and matching Parquet schema/reader");
    let all_null = run_query(connection, "SELECT CAST(NULL AS BIGINT) AS all_null").await?;
    require(
        all_null.row_count() == 1 && all_null.batches()[0].column(0).is_null(0),
        "all-NULL real row was discarded",
    )?;
    println!("PASS all-NULL real row retained");
    check_extended_schemas(connection).await?;
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

fn check_empty_result(
    result: QueryResult,
    expected: &arrow_schema::SchemaRef,
) -> Result<(), String> {
    require(result.row_count() == 0, "empty result returned rows")?;
    require(
        &result.schema() == expected,
        "empty result's full Parquet schema differs",
    )?;
    let (reader, warnings) = result.into_reader_with_warnings();
    require(
        warnings.is_empty(),
        "unexpected session warning during acceptance",
    )?;
    require(&reader.schema() == expected, "empty reader lost its schema")?;
    for batch in reader {
        require(
            batch.map_err(|_| "cannot read empty result")?.num_rows() == 0,
            "schema-recovery rows escaped into the result",
        )?;
    }
    Ok(())
}

async fn check_extended_schemas(connection: &Connection) -> Result<(), String> {
    for (label, sql, columns) in [("scalar", SCALAR_PROBE, 17), ("nested", NESTED_PROBE, 9)] {
        let baseline = run_query(connection, sql).await?;
        require(
            baseline.row_count() == 1 && baseline.schema().fields().len() == columns,
            "extended literal shape changed",
        )?;
        let schema = baseline.schema();
        for empty_sql in [
            format!("SELECT * FROM (\n{sql}\n) q WHERE FALSE"),
            format!("{sql}\nLIMIT 0"),
            format!("WITH invented AS (\n{sql}\n) SELECT * FROM invented OFFSET 1"),
        ] {
            check_empty_result(run_query(connection, &empty_sql).await?, &schema)?;
        }
        if label == "nested" {
            // A real row with NULL nested parents must not be treated as a
            // recovery placeholder. Selection keeps all nested types intact.
            let all_null = run_query(connection, &format!(
                "SELECT q.* FROM (VALUES (1)) seed(unused) LEFT JOIN (SELECT * FROM (\n{sql}\n) original LIMIT 0) q ON TRUE"
            )).await?;
            require(
                all_null.row_count() == 1 && all_null.schema() == schema,
                "all-NULL nested row or schema changed",
            )?;
            require(
                all_null.batches().iter().all(|batch| {
                    batch
                        .columns()
                        .iter()
                        .all(|array| array.null_count() == batch.num_rows())
                }),
                "expected invented NULL nested parents",
            )?;
        }
        println!(
            "PASS {label}: full schema retained for filtering, LIMIT 0 and CTE/OFFSET empties"
        );
    }
    Ok(())
}

async fn load_sdk_config(profile: Option<String>, region: Option<String>) -> SdkConfig {
    let mut loader = aws_config::defaults(BehaviorVersion::latest());
    if let Some(profile) = profile {
        loader = loader.profile_name(profile);
    }
    if let Some(region) = region {
        loader = loader.region(Region::new(region));
    }
    loader.load().await
}

async fn run(args: Args) -> Result<(), String> {
    run_with_loader(args, load_sdk_config).await
}

async fn run_with_loader<F, Fut>(args: Args, load: F) -> Result<(), String>
where
    F: FnOnce(Option<String>, Option<String>) -> Fut,
    Fut: std::future::Future<Output = SdkConfig>,
{
    // Validate all offline inputs before resolving credentials or making clients.
    let deployment = args
        .deployment
        .as_ref()
        .map(|path| {
            let file = std::fs::File::open(path).map_err(|_| "cannot read deployment contract")?;
            let deployment = read_contract(file)?;
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
    let region = deployment
        .as_ref()
        .map(|deployment| deployment.region.clone())
        .or(args.region);
    let base = load(args.profile, region).await;
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
                .any(|role| operator_matches(&identity, role)),
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
            let mut options = ConnectionOptions::new(&seat.workgroup, &deployment.database);
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
            let workgroup = connection
                .workgroup_configuration()
                .await
                .map_err(safe_core_error)?;
            validate_workgroup(&workgroup, &seat, &deployment.account_id)?;
            println!("PASS {name}: expected synthetic account, reader and workgroup fence");
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
    fn fixed_probe_accepts_rows_in_either_order_across_batches() {
        use arrow_array::{ArrayRef, RecordBatch, types::Int32Type};
        use std::sync::Arc;
        let list =
            ListArray::from_iter_primitive::<Int32Type, _, _>([None, Some(vec![Some(1), Some(2)])]);
        let batch = RecordBatch::try_from_iter(vec![
            ("id", Arc::new(Int32Array::from(vec![2, 1])) as ArrayRef),
            (
                "s_null",
                Arc::new(StringArray::from(vec![Some("a"), None])) as _,
            ),
            ("s_empty", Arc::new(StringArray::from(vec!["b", ""])) as _),
            (
                "big",
                Arc::new(Int64Array::from(vec![None, Some(9_007_199_254_740_993)])) as _,
            ),
            (
                "dec",
                Arc::new(
                    Decimal128Array::from(vec![None, Some(110)])
                        .with_precision_and_scale(3, 2)
                        .unwrap(),
                ) as _,
            ),
            (
                "ts",
                Arc::new(PrimitiveArray::<TimestampMillisecondType>::from(vec![
                    None,
                    Some(1_792_154_096_789),
                ])) as _,
            ),
            ("arr", Arc::new(list) as _),
        ])
        .unwrap();
        check_probe_batches(&batch.schema(), &[batch.slice(0, 1), batch.slice(1, 1)]).unwrap();
        check_probe_batches(&batch.schema(), &[batch.slice(1, 1), batch.slice(0, 1)]).unwrap();
    }
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
    fn args_without_live() -> Args {
        Args::try_parse_from([
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
        .unwrap()
    }

    #[tokio::test]
    async fn offline_gate_executes_before_the_sdk_loader() {
        let error = run_with_loader(args_without_live(), |_, _| async {
            panic!("SDK configuration must not be loaded")
        })
        .await
        .unwrap_err();
        assert!(error.contains("no AWS clients created"));
    }

    fn deployment_contract() -> serde_json::Value {
        serde_json::json!({
            "format_version": 1, "purpose": "ckdata-synthetic-tests",
            "account_id": "000000000000", "region": "us-west-2",
            "bucket": "ck-datalake-tst-ckdata-fixtures-000000000000",
            "database": "ck_datalake_tst_ckdata_fixtures", "table": "types",
            "fixture_keys": ["fixtures/types/part-1.parquet", "fixtures/types/part-2.parquet"],
            "operator_role_arns": ["arn:aws:iam::111111111111:role/operator"],
            "seats": {
                "internal": {
                    "role_arn": "arn:aws:iam::000000000000:role/ck-datalake-tst-ckdata-internal-reader",
                    "source_role_arn": null, "workgroup": "ck-datalake-tst-ckdata-internal",
                    "output_location": "s3://ck-datalake-tst-ckdata-fixtures-000000000000/results/internal/"
                },
                "research": {
                    "role_arn": "arn:aws:iam::000000000000:role/ck-datalake-tst-ckdata-research-reader",
                    "source_role_arn": "arn:aws:iam::000000000000:role/ck-datalake-tst-ckdata-source",
                    "workgroup": "ck-datalake-tst-ckdata-research",
                    "output_location": "s3://ck-datalake-tst-ckdata-fixtures-000000000000/results/research/"
                }
            }
        })
    }

    #[tokio::test]
    async fn invalid_contract_executes_before_the_sdk_loader() {
        let contract = deployment_contract();
        let deployment: Deployment = serde_json::from_value(contract.clone()).unwrap();
        assert!(validate(&deployment, None).is_ok());
        for (field, invalid) in [("region", "us-east-1"), ("bucket", "near-miss-tst-bucket")] {
            let path =
                std::env::temp_dir().join(format!("ckdata-invalid-{}.json", uuid::Uuid::new_v4()));
            let mut changed = contract.clone();
            changed[field] = serde_json::json!(invalid);
            std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
            let mut args = args_without_live();
            args.run_live = true;
            args.deployment = Some(path.clone());
            let result = run_with_loader(args, |_, _| async {
                panic!("invalid contract must not load SDK configuration")
            })
            .await;
            std::fs::remove_file(path).unwrap();
            assert!(result.is_err());
        }
    }

    #[test]
    fn contract_read_is_bounded_before_json_allocation() {
        struct LargeInput {
            count: std::rc::Rc<std::cell::Cell<usize>>,
        }
        impl Read for LargeInput {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                self.count.set(self.count.get() + buffer.len());
                buffer.fill(b' ');
                Ok(buffer.len())
            }
        }
        let count = std::rc::Rc::new(std::cell::Cell::new(0));
        assert_eq!(
            read_contract(LargeInput {
                count: count.clone()
            })
            .unwrap_err(),
            "deployment contract is too large"
        );
        assert_eq!(count.get(), 65537);
    }

    #[test]
    fn admitted_operators_can_be_in_another_account() {
        let identity = CallerIdentity {
            account: "111111111111".into(),
            arn: "arn:aws:sts::111111111111:assumed-role/operator/session".into(),
        };
        assert!(operator_matches(
            &identity,
            "arn:aws:iam::111111111111:role/path/operator"
        ));
        assert!(!operator_matches(
            &identity,
            "arn:aws:iam::000000000000:role/operator"
        ));
        for invalid in [
            "arn:aws:iam::111111111111:role/*",
            "arn:aws:iam::111111111111:root",
            "arn:aws:iam::111111111111:role/",
        ] {
            assert!(role_account(invalid).is_none());
        }
    }

    fn fixture_seat() -> Seat {
        Seat {
            output_location: "s3://synthetic/results/internal/".into(),
            role_arn: "arn:aws:iam::000000000000:role/synthetic".into(),
            source_role_arn: None,
            workgroup: "synthetic".into(),
        }
    }

    fn fixture_workgroup() -> WorkGroupConfiguration {
        use aws_sdk_athena::types::{EncryptionConfiguration, EngineVersion, ResultConfiguration};
        WorkGroupConfiguration::builder()
            .enforce_work_group_configuration(true)
            .bytes_scanned_cutoff_per_query(10 * 1024 * 1024)
            .engine_version(
                EngineVersion::builder()
                    .selected_engine_version("Athena engine version 3")
                    .build(),
            )
            .result_configuration(
                ResultConfiguration::builder()
                    .output_location("s3://synthetic/results/internal/")
                    .expected_bucket_owner("000000000000")
                    .encryption_configuration(
                        EncryptionConfiguration::builder()
                            .encryption_option(EncryptionOption::SseS3)
                            .build()
                            .unwrap(),
                    )
                    .build(),
            )
            .build()
    }

    #[test]
    fn live_workgroup_drift_rejects_before_query_submission() {
        let seat = fixture_seat();
        let config = fixture_workgroup();
        assert!(validate_workgroup(&config, &seat, "000000000000").is_ok());
        let mut missing_cutoff = config.clone();
        missing_cutoff.bytes_scanned_cutoff_per_query = None;
        let mut excessive_cutoff = config.clone();
        excessive_cutoff.bytes_scanned_cutoff_per_query = Some(10 * 1024 * 1024 + 1);
        let mut unenforced = config.clone();
        unenforced.enforce_work_group_configuration = Some(false);
        let mut output_drift = config.clone();
        output_drift
            .result_configuration
            .as_mut()
            .unwrap()
            .output_location = Some("s3://other/results/".into());
        let mut owner_drift = config.clone();
        owner_drift
            .result_configuration
            .as_mut()
            .unwrap()
            .expected_bucket_owner = Some("111111111111".into());
        let mut missing_encryption = config;
        missing_encryption
            .result_configuration
            .as_mut()
            .unwrap()
            .encryption_configuration = None;
        let changes = [
            missing_cutoff,
            excessive_cutoff,
            unenforced,
            output_drift,
            owner_drift,
            missing_encryption,
        ];
        for changed in changes {
            let submissions = std::cell::Cell::new(0);
            let result = validate_workgroup(&changed, &seat, "000000000000")
                .map(|()| submissions.set(submissions.get() + 1));
            assert!(result.is_err());
            assert_eq!(submissions.get(), 0);
        }
    }

    #[tokio::test]
    async fn deployment_drift_stops_the_real_run_before_start_query() {
        use aws_config::retry::RetryConfig;
        use aws_smithy_async::time::StaticTimeSource;
        use aws_smithy_http_client::test_util::{ReplayEvent, StaticReplayClient};
        use aws_smithy_types::body::SdkBody;
        for (field, value, reason) in [
            (
                "OutputLocation",
                serde_json::json!("s3://other/results/"),
                "workgroup output differs",
            ),
            (
                "BytesScannedCutoffPerQuery",
                serde_json::Value::Null,
                "workgroup scan cutoff differs",
            ),
        ] {
            let mut configuration = serde_json::json!({
                "EnforceWorkGroupConfiguration": true,
                "BytesScannedCutoffPerQuery": 10485760,
                "EngineVersion": {"SelectedEngineVersion": "Athena engine version 3"},
                "ResultConfiguration": {
                    "OutputLocation": "s3://ck-datalake-tst-ckdata-fixtures-000000000000/results/internal/",
                    "ExpectedBucketOwner": "000000000000",
                    "EncryptionConfiguration": {"EncryptionOption": "SSE_S3"}
                }
            });
            if field == "OutputLocation" {
                configuration["ResultConfiguration"][field] = value;
            } else {
                configuration[field] = value;
            }
            let operator = r#"<GetCallerIdentityResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/"><GetCallerIdentityResult><Account>111111111111</Account><Arn>arn:aws:sts::111111111111:assumed-role/operator/session</Arn></GetCallerIdentityResult></GetCallerIdentityResponse>"#;
            let assumed = r#"<AssumeRoleResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/"><AssumeRoleResult><Credentials><AccessKeyId>SYNTHETIC</AccessKeyId><SecretAccessKey>synthetic-secret</SecretAccessKey><SessionToken>synthetic-token</SessionToken><Expiration>2024-01-01T01:00:00Z</Expiration></Credentials></AssumeRoleResult></AssumeRoleResponse>"#;
            let reader = r#"<GetCallerIdentityResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/"><GetCallerIdentityResult><Account>000000000000</Account><Arn>arn:aws:sts::000000000000:assumed-role/ck-datalake-tst-ckdata-internal-reader/session</Arn></GetCallerIdentityResult></GetCallerIdentityResponse>"#;
            let group = serde_json::json!({"WorkGroup": {"Name": "ck-datalake-tst-ckdata-internal", "State": "ENABLED", "Configuration": configuration}}).to_string();
            let http = StaticReplayClient::new(
                [
                    ("text/xml", operator),
                    ("text/xml", assumed),
                    ("text/xml", reader),
                    ("application/x-amz-json-1.1", group.as_str()),
                ]
                .into_iter()
                .map(|(kind, body)| {
                    ReplayEvent::new(
                        http::Request::new(SdkBody::empty()),
                        http::Response::builder()
                            .status(200)
                            .header("content-type", kind)
                            .body(SdkBody::from(body))
                            .unwrap(),
                    )
                })
                .collect(),
            );
            let config = SdkConfig::builder()
                .behavior_version(BehaviorVersion::latest())
                .region(Region::new("us-west-2"))
                .credentials_provider(SharedCredentialsProvider::new(Credentials::new(
                    "SYNTHETIC",
                    "synthetic-secret",
                    None,
                    None,
                    "synthetic-test",
                )))
                .time_source(StaticTimeSource::from_secs(1_704_067_200))
                .retry_config(RetryConfig::standard().with_max_attempts(1))
                .http_client(http.clone())
                .build();
            let path =
                std::env::temp_dir().join(format!("ckdata-drift-{}.json", uuid::Uuid::new_v4()));
            std::fs::write(&path, serde_json::to_vec(&deployment_contract()).unwrap()).unwrap();
            let mut args = args_without_live();
            args.run_live = true;
            args.deployment = Some(path.clone());
            let error = run_with_loader(args, |_, _| async { config })
                .await
                .unwrap_err();
            std::fs::remove_file(path).unwrap();
            assert!(error.contains(reason), "{error}");
            let requests: Vec<_> = http.actual_requests().collect();
            assert_eq!(requests.len(), 4);
            assert_eq!(
                requests[3].headers().get("x-amz-target"),
                Some("AmazonAthena.GetWorkGroup")
            );
        }
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
