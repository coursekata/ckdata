# Development

Run commands from the repository root. Package installation and AWS
configuration are in the [installation guide](installation.md); researcher
access is documented internally by CourseKata.

## Repository structure

| Path | Responsibility |
|---|---|
| `core/` | Shared Rust Athena client, Parquet reader and Arrow results; crate `ckdata` |
| `r/` | R DBI/dbplyr package and extendr adapter; crate `ckdata-r` |
| `python/` | Python package and PyO3 adapter, built with maturin; crate `ckdata-py`, import `ckdata` |
| `tests/` | Tests of repository maintenance tooling |
| `scripts/` | Repository maintenance tooling |

Language adapters own host interfaces and conversions. The core owns AWS
queries and result retrieval. The R crate has an independent workspace;
`r/src/rust/core` links to the shared core, which `R CMD build` copies into the
source tarball.

## Setup and commands

Install [Just](https://just.systems/man/en/) 1.46 or later,
[Rust through rustup](https://rustup.rs) 1.94.1 or later, R 4.2 or later,
[uv](https://docs.astral.sh/uv/getting-started/installation/), Node.js/npm,
[Actionlint](https://github.com/rhysd/actionlint#installation),
[Gitleaks](https://github.com/gitleaks/gitleaks#installing) and jq. Native builds
also need the platform prerequisites in the installation guide. Put these
executables on PATH; Just can be installed with `uv tool install rust-just`.

```sh
just setup
just
```

Setup installs rustfmt and Clippy, fetches locked Cargo dependencies, prepares
the uv environment, installs the R package and development dependencies with
pak, and installs Lefthook's Git hooks. It uses the normal R library settings
and can be rerun. It does not authenticate to AWS or provision resources.

| Command | Purpose |
|---|---|
| `just fmt` | Apply rustfmt, Ruff and Air, plus whitespace and end-of-file fixes |
| `just fmt --check` | Check formatting |
| `just lint` | Run static analysis and repository checks |
| `just lint --fix` | Apply supported automatic fixes and report remaining findings |
| `just test` | Run offline Rust, R, Python and repository-tooling tests |
| `just check` | Run `fmt --check`, `lint` and `test` |
| `just build` | Build an R source archive and Python source/wheel distributions in `dist/` |
| `just build --check` | Build distributions, run `R CMD check` and test the installed wheel |
| `just docs` | Regenerate native R wrappers and R documentation |
| `just test-acceptance` | Run the configured synthetic AWS suite with automatic login disabled |

`lint --target` accepts `rust`, `r`, `python` or `repo`; `test --target` accepts
`rust`, `r`, `python` or `tooling`. Both default to `all`. Clippy's automatic
fixes include uncommitted files; they do not stage changes. R lint findings
require manual changes.

Just owns commands and prerequisites. Lefthook owns Git events: commits check
formatting and scan staged contents for secrets; pushes run `just check`.
Hooks use check mode. Setup removes old prek-generated hooks; Lefthook handles
backups of unrelated hooks. Editors can invoke the native formatters on save;
the repository has no editor-specific configuration.

The uv development group pins Just, Lefthook, Ruff, Air and file checkers.
`.github/package.json` and its lockfile own the Renovate validator and Node 24
runtime. Setup installs that runtime locally and builds its regex dependency.
[Dependency maintenance](../.github/dependency_policy.md) records update
ownership, scanners and upstream constraints.

### Native build caches

Cargo outputs default to the root `target/`, outside the R source package.
Setup clears the legacy `r/src/rust/target/` cache if present. R copies source
before applying `.Rbuildignore`, so a cache inside `r/` can exhaust temporary
disk space. `build` fails early if it reappears. Keep any explicit
`CARGO_TARGET_DIR` outside `r/` too.

### R and Python adapters

Run `just docs` after changing an extendr export or R function documentation.
It runs the extendr generator from `r/src`, then roxygen2 through devtools.
`r/tools/config.R` configures platform flags and native build caches; Cargo
enforces the minimum Rust version.

Python requires 3.11 or later. The root `pyproject.toml` and `uv.lock` define
one development environment. The `bindings` group supplies maturin, PyArrow,
dataframe libraries and pytest. Setup and Python tests install locked build
tools through uv before compiling the binding. Distribution builds use the
backend's isolated build path.

### Windows

Enable Developer Mode and clone with symlinks enabled so the shared core's
link is preserved:

```sh
git clone -c core.symlinks=true https://github.com/coursekata/ckdata.git
```

Install matching Rtools and Rust's GNU target as described in the installation
guide. Use hosted Windows CI to verify Windows behavior from macOS or Linux;
Docker Desktop on macOS runs a Linux VM, not Windows.

## Testing

Tests stay with the code they exercise. The shared core uses Cargo, the R
package uses testthat and the Python binding uses pytest. Root `tests/unit/`
covers repository release and license tooling. The R crate's native tests run
separately from the root Cargo workspace.

`just check` runs the offline suite. `just build --check` checks the actual R
source archive and tests the installed Python wheel in a temporary environment.
There is no separate R build simulator or tarball checker. CI starts Linux,
macOS and Windows R checks as soon as the source archive is ready; Rust lint,
core tests and Python tests run independently. Every job is required. Native
caches are saved after failures so completed compilation can be reused.

R tests cover read-side DBI behavior, dbplyr translations, parameter binding,
types, Arrow ownership, saved results, authentication and credential renewal.
They use invented native fixtures and SQLite tables. Private native fixture
entrypoints remain in the installed adapter so tests can exercise real cursors,
streams and asynchronous jobs. The core tests Parquet retrieval with fake AWS
responses. Writes and transactions are outside the read-only backend's scope.

### Synthetic AWS smoke tests

The opt-in `r/tests/testthat/test_live_*.R` tests exercise the real reader against
invented data. Provisioning and fixture permission tests belong to ck-datalake's
independent `environments/ckdata-test` environment. Its deployment contract
identifies the restricted roles, workgroups and invented `types` table.

With the local package installed, supply the contract and selected seat:

```sh
CKDATA_TEST_DEPLOYMENT=/absolute/path/to/ckdata-test-deployment.json \
CKDATA_TEST_SEAT=internal just test-acceptance
```

Use `research` for the other seat with its appropriate AWS source identity.
The suite verifies the configured account, role, workgroup, schema and output
protection before querying. It covers metadata, nested and empty results, exact
integers, temporal values, DBI paging, binding, joins and saved Parquet results.
It never reads real research tables or deletes AWS outputs. Missing credentials
fail immediately; this command never opens sign-in. Ordinary tests skip live
queries before credential discovery unless `CKDATA_RUN_LIVE_TESTS=true`.

The core's smoke example uses the same deployment contract and fixed queries:

```sh
cargo run -p ckdata --example query_acceptance -- --help
cargo run -p ckdata --example query_acceptance -- \
  --deployment /absolute/path/to/ckdata-test-deployment.json --run-live
```

It validates the contract before authenticating, checks the restricted reader
identities and tests the result path without printing rows. There is no arbitrary
SQL option. Credential renewal uses offline tests with a virtual clock.
Real-role/enclave checks and elapsed-hour expiry remain final BB-6313 acceptance;
any real-data query needs Adam's prior approval and must use aggregates without PII.

The remaining manual smoke check is browser launch: from an interactive R session
with synthetic SSO settings and expired or absent credentials, the first
connection should open AWS sign-in and resume after it completes. Package loading
must not authenticate. Browser process handling, cancellation, precedence and
renewal are covered by unit tests; `login = FALSE` suppresses browser interaction.

## Shared core API and defaults

`Connection::query` takes SQL, `QueryOptions` and a `QueryCancellation` token,
returning a `QueryResult` with a Parquet schema and owned Arrow batches. The core
trims whitespace and one trailing semicolon before wrapping SQL in UNLOAD.
Athena validates the SQL; the paginated manifest API supplies file paths and the
upstream Parquet reader decodes them. Fileless results use one schema-only
UNLOAD under the original timeout and cancellation signal. Failures retain the
original query ID and recovery error.

Signal cancellation and keep awaiting the query so the client can request a
remote stop. The core installs no global Ctrl-C handler. `into_reader()`
transfers the Arrow reader. AWS providers renew credentials; hosts service
`SsoLogin` requests without restarting the native query job. The core does not
sort results or promise an out-of-core memory bound.

The R adapter maps legacy environment aliases in `r/R/settings.R` without
rewriting `AWS_*` settings. Managed helpers and DBI use the same defaults;
Rust receives explicit settings. No CourseKata account, role, region, output
bucket, freshness schedule or production database is selected automatically.
A workgroup is required; schema defaults to `default` and the catalog is
`AwsDataCatalog`. The Python query configuration interface is still pending.

## Releases

R releases use CourseKata's R-universe registry with `subdir: r` and
`branch: "*release"`. Python wheel publication is pending.

1. Update `core/Cargo.toml`, `python/Cargo.toml`, `r/src/rust/Cargo.toml`,
   `r/DESCRIPTION` and `r/NEWS.md` together. Refresh the native lockfiles with
   `cargo update --workspace` in both workspaces.
2. Run `python3 scripts/check_licenses.py --write` and review the notice changes.
   Merge the version change and wait for successful CI on that main commit.
3. Create and push a lightweight `vX.Y.Z` tag pointing to that commit. The release
   workflow requires `R_RELEASE_READY=true`, matching versions and successful
   main CI before creating the GitHub release.
4. R-universe builds the binaries. The workflow waits for them, installs without
   Rust and runs the installed R tests on current R for Ubuntu 26.04 x86_64,
   macOS arm64 and Windows x86_64. Linux must match R-universe's distribution.

Never move a release tag; release a new version for a fix. The
[`verify_release` workflow](../.github/workflows/verify_release.yml) can repeat
binary verification for an existing version. When changing the release compiler,
run `python3 scripts/check_licenses.py --runtime-write --write` and review its
provenance and licenses. `--archive /absolute/path/to/package` checks notices in
a wheel, source archive or R tarball.
