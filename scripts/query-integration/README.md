# Synthetic query integration checks

These developer harnesses use the reusable fixture environment owned by `coursekata/ck-datalake`, under `environments/ckdata-test`. The environment contains only invented rows and test-scoped identities. The Rust example exercises the implemented reader; the Python probe provides an independent transport comparison.

## Rust acceptance

```sh
cargo run -p ckdata --example query-acceptance -- --help
cargo run -p ckdata --example query-acceptance -- \
  --deployment /absolute/path/to/ckdata-test-deployment.json \
  --profile YOUR_OPERATOR_PROFILE --run-live
```

The contract is validated before credentials are resolved. The operator must match an admitted role. The harness assumes each test chain in memory, verifies the selected identity before querying, and runs fixed literal, empty, all-NULL-row, metadata and invented-table checks. It prints assertion outcomes without result rows and never deletes outputs. Ctrl-C requests query cancellation. No arbitrary SQL option exists.

Add `--wait-for-expiry` to wait for the real one-hour explicit-role lifetime and confirm that new queries are rejected. That wait is manual acceptance, not an ordinary CI test. Without the flag, virtual-clock Rust tests cover expiry.

Controlled final acceptance uses `--workgroup`, `--schema`, `--expected-account`, `--expected-role`, optional `--role-arn` and optional catalog-only `--table`, together with standard `--profile`/`--region` and the explicit `--run-live` flag. It uses only the fixed invented literal queries; the supplied table is never selected. Run the research check from the enclave's existing identity. No production trust exception is needed.

The core preserves an empty result's Parquet schema by adding a private presence column inside a singleton outer join. A zero-row result emits one all-NULL presence row; the reader removes it and its private column. Actual all-NULL rows carry a true marker and survive. This avoids guessing nested Arrow types from a file-free manifest. A missing schema-bearing file is an error.

Query results are bounded collections. Defaults are 1 MiB of manifest data, 256 files, 64 MiB per file, 256 MiB downloaded, 256 MiB of decoded arrays, one million rows and four concurrent downloads. Sorting can temporarily need additional copies and row indices; these are collection limits, not a hard process-memory cap. `QueryOptions.result_limits` allows deliberate adjustment. The reader follows manifest entries only, checks server-side encryption, and requires a full key ARN for workgroup SSE-KMS comparisons. Unsupported timezone-bearing timestamps and duplicate names are explicit errors. Final ordering of returned scalar columns/aliases/ordinals is restored; an unavailable or unsupported sort key returns an `OrderNotRestored` warning.

## Independent Python probe

Obtain the environment's nonsecret deployment contract with `tofu output -json ckdata_test` from its own infrastructure state. Keep the resulting file outside Git. Using a standard AWS session for an operator admitted by that environment:

```sh
uv run --locked scripts/query-integration/check.py --run-live \
  --deployment /absolute/path/to/ckdata-test-deployment.json
```

The contract supplies the region, account, exact role chain, workgroups, database and fixed table. There are no arbitrary SQL or production-role options. The harness rejects non-test namespaces before creating data clients, verifies the source and every assumed identity, then queries as each restricted test reader. Credentials remain in memory.

For each seat it checks literal types, an empty query and the named invented Parquet fixture, including NULL, empty string, exact int64, decimal, timestamp and array values. Table/column discovery uses metadata APIs without submitting a query. It also requires denied access to the other test workgroup and to a known fixture object's source bytes. Manifest entries and downloads are confined to the generated output prefixes. Results must match the workgroup's encryption; only assertion outcomes, zero-row representation and safe failure identifiers are printed.

Three bounded UNLOAD queries run per seat. Athena writes fresh outputs under that seat's test prefix; the environment expires them after three days. The harness has no deletion or infrastructure permissions. Its unwrapped zero-row query records a manifest with no files and no Parquet schema; the Rust reader uses the presence-column strategy described above.

Run the same transport and safety checks with fake services, without constructing AWS clients:

```sh
uv run --locked scripts/query-integration/check.py --self-test
```

CI runs only this offline mode. Real internal/enclave-role checks and actual one-hour expiry remain final BB-6315 acceptance after the Rust query path works. This harness deliberately cannot run against those production roles.
