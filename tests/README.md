# Tests

These developer harnesses use the reusable fixture environment owned by `coursekata/ck-datalake`, under `environments/ckdata-test`. The environment contains only invented rows and test-scoped identities. The Rust example exercises the implemented reader; the Python probe provides an independent transport comparison.

## Layout and commands

Python checks use pytest and `test_*.py` filenames. Maintained Python and Rust
source filenames use `snake_case`. Tests live under `tests/`; developer commands
live under `scripts/`. R's generated wrappers, configure entry points and other
framework-owned files retain their required names.

- `tests/unit/`: audit reporting, release guards, archive/license checks and the
  query probe with fake services. No network, AWS credentials, R or native build.
- `tests/integration/`: small R/Cargo build fixtures, including cache ownership,
  spaces in paths, failure cleanup and simulated Windows recipes. Requires R,
  Cargo and make; uses no AWS services.
- `python/tests/`: compiled binding handoffs to frame libraries. Requires the
  optional `bindings` dependency group and the native package.
- `core/` and `r/tests/testthat/`: native Rust and R tests use their language's
  standard runners, Cargo and testthat.

From the repository root:

```sh
uv run --locked pytest                         # all developer-tool tests
uv run --locked pytest tests/unit              # fast tests
uv run --locked pytest tests/integration       # R build integration tests
uv run --locked --group bindings --all-packages pytest python/tests
cargo test --workspace --all-targets --locked --exclude ckdata-py
Rscript -e 'devtools::test("r")'
```

`uv.lock` pins the shared Python development environment, across both dependency
groups and supported Python/platform branches. It does not belong to an individual
test and is not installed with either public package. The binding command forces its editable rebuild so changed Rust sources are
compiled before testing. Cargo still reuses unchanged dependencies. The two Cargo locks remain
separate because the R source package carries an independently buildable crate.

CI runs the fast tests before expensive native package checks and runs the R
build tests in the job that installs R. No test runs on import. All live AWS
access is confined to the explicit command below.

## Rust acceptance

```sh
cargo run -p ckdata --example query_acceptance -- --help
cargo run -p ckdata --example query_acceptance -- \
  --deployment /absolute/path/to/ckdata-test-deployment.json \
  --profile YOUR_OPERATOR_PROFILE --run-live
```

The contract read stops at 64 KiB and is validated before credentials are resolved. The operator must match an admitted role. The harness assumes each test chain in memory, verifies the selected identity before querying, and runs fixed literal, empty, all-NULL-row, metadata and invented-table checks. It prints assertion outcomes without result rows and never deletes outputs. Ctrl-C requests query cancellation. No arbitrary SQL option exists.

Add `--wait-for-expiry` to wait for the real one-hour explicit-role lifetime and confirm that new queries are rejected. That wait is manual acceptance, not an ordinary CI test. Without the flag, virtual-clock Rust tests cover expiry.

Controlled final acceptance uses `--workgroup`, `--schema`, `--expected-account`, `--expected-role`, optional `--role-arn` and optional catalog-only `--table`, together with standard `--profile`/`--region` and the explicit `--run-live` flag. It uses only the fixed invented literal queries; the supplied table is never selected. Run the research check from the enclave's existing identity. No production trust exception is needed.

The core preserves an empty result's Parquet schema by adding a private presence column inside a singleton outer join. A zero-row result emits one all-NULL presence row; the reader removes it and its private column. Actual all-NULL rows carry a true marker and survive. This avoids guessing nested Arrow types from a file-free manifest. A missing schema-bearing file is an error.

Query results are bounded collections. Defaults are 1 MiB of manifest data, 256 files, 64 MiB per file, 256 MiB downloaded, 256 MiB of decoded arrays, one million rows and four concurrent downloads. Pages are capped at 8 MiB, headers at 64 KiB and footers at 1 MiB. Schemas are limited to 4096 physical nodes and depth 32. Dictionary and repeated-value expansion is conservatively checked before Arrow allocation; a skewed dictionary can require a deliberately larger decoded limit. PLAIN and dictionary byte-array encodings are supported; prefix/delta byte-array encodings and fixed widths above 32 bytes are explicit errors. Sorting can temporarily need additional copies and row indices; these are collection limits, not a hard process-memory cap. `QueryOptions.result_limits` allows deliberate adjustment. The reader follows manifest entries only and checks server-side encryption. SSE-KMS accepts a full key ARN or standard key UUID; aliases are rejected before submission because object identity cannot be verified from them. Unsupported timezone-bearing timestamps and duplicate names are explicit errors. Final ordering of returned scalar columns/aliases/ordinals is restored; an unavailable or unsupported sort key returns an `OrderNotRestored` warning.

## Independent Python probe

Obtain the environment's nonsecret deployment contract with `tofu output -json ckdata_test` from its own infrastructure state. Keep the resulting file outside Git. Using a standard AWS session for an operator admitted by that environment:

```sh
uv run --locked python -m scripts.query_probe --run-live \
  --deployment /absolute/path/to/ckdata-test-deployment.json
```

The contract supplies the region, account, exact role chain, workgroups, database and fixed table. There are no arbitrary SQL or production-role options. The harness rejects non-test namespaces before creating data clients, verifies the source and every assumed identity, then queries as each restricted test reader. Credentials remain in memory.

For each seat it checks literal types, an empty query and the named invented Parquet fixture, including NULL, empty string, exact int64, decimal, timestamp and array values. Table/column discovery uses metadata APIs without submitting a query. It also requires denied access to the other test workgroup and to a known fixture object's source bytes. Manifest entries and downloads are confined to the generated output prefixes. Results must match the workgroup's encryption; only assertion outcomes, zero-row representation and safe failure identifiers are printed.

Three bounded UNLOAD queries run per seat. Athena writes fresh outputs under that seat's test prefix; the environment expires them after three days. The harness has no deletion or infrastructure permissions. Its unwrapped zero-row query records a manifest with no files and no Parquet schema; the Rust reader uses the presence-column strategy described above.

Run the same transport and safety checks with fake services:

```sh
uv run --locked pytest tests/unit/test_query_probe.py
```

Real internal/enclave-role checks and actual one-hour expiry remain final
BB-6315 acceptance. The Python probe cannot run against those production roles.

The synthetic deployment contract is fixed to `us-west-2`, the generated fixture
namespace, and both seat identities. Before submitting a query, each harness checks
the live workgroup's exact output prefix, expected bucket owner, SSE-S3 encryption,
enforced configuration, Athena engine 3, and 10 MiB scan cutoff. Operator roles may
belong to another account when their exact IAM ARNs are admitted by the contract.

Renovate maintains the normal `pyproject.toml` declarations and shared `uv.lock`,
including monthly lock maintenance. The seven-day wait is enforced by
`tool.uv.exclude-newer`; no custom lock-update workflow is needed. To refresh
manually, run `uv lock --upgrade`, then `uv run --locked pytest`.

CI exports every public Python lock record, including optional groups and platform
branches, into a scanner-only inventory alongside both Cargo locks. The scanner's
coverage is verified. All severities and unfixed advisories remain in the retained
report; fixed HIGH/CRITICAL advisories fail the merge gate. This export is never
used as an installation requirements file.

Both harnesses accept millisecond, microsecond and nanosecond Arrow storage for
naive timestamps and require the exact expected instant. Athena INT96 output can
use a different storage unit from the fixture input. Width, decimal precision
and scale, array element type, NULLs and timezone changes remain checked.

Successful core queries include `SessionExpiring` in their warnings during the
last fifteen minutes of an explicit role lease. Hosts must present those warnings;
`into_reader_with_warnings` preserves them during Arrow handoff. Service errors
retain modeled operational guidance. Raw responses, credential-provider details
and signed authentication diagnostics are excluded.
