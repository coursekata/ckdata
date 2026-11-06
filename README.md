# ckdata

ckdata provides a read-only R DBI/dbplyr interface to the CourseKata data lake. A Rust core executes Athena queries and reads Parquet results into Arrow. The Python query interface is still in development.

## Status

This is an unpublished development build. The R package supports lazy queries, SQL, typed empty results and explicit local caching. Use [ckdata-r](https://github.com/coursekata/ckdata-r) for the published package until this replacement completes acceptance and its first binary release. Python wheels and PyPI publication follow full Python package testing.

The Rust core supports standard AWS connection settings, a fixed-lifetime assumed role and table/column discovery through Athena's metadata APIs. Queries use UNLOAD with a fresh workgroup output prefix and support cancellation. The core retrieves file paths through Athena's paginated manifest API and reads the files with the upstream Parquet reader. Athena writes UNLOAD files in parallel, so combined row order is not guaranteed, even with `ORDER BY`. Tests use invented configuration and mocked HTTP services.

## Install

This replacement is not yet available from CourseKata's R-universe. To try the local source package, install R 4.2 or later and [Rust through rustup](https://rustup.rs), then run from the repository root:

```sh
Rscript -e 'install.packages("pak"); pak::local_install("r")'
```

Source installation requires Rust 1.94.1 or later, a C compiler, make and xz. The planned binary checks cover the current R release on Ubuntu 26.04 x86_64, macOS arm64 and Windows x86_64; other systems and R versions require source installation. Binary installation commands will be added after those checks pass against the first release.

On Windows, install the [Rtools version matching your R installation](https://cran.r-project.org/bin/windows/Rtools/) and add Rust's GNU target with `rustup target add x86_64-pc-windows-gnu`. Clone with symlinks enabled as described under [Developing](#developing).

## R usage

Sign in using your organization's AWS process, then select a standard AWS profile and region. Set `CK_DATALAKE_WORKGROUP` and `CK_DATALAKE_SCHEMA` for the intended environment; the workgroup supplies the result location and encryption. See `?ckdata::ckdata_settings` for connection options.

```r
library(ckdata)
library(dplyr)

query <- lake_tbl("my_table") |>
  filter(category == "example") |>
  summarise(n = n())

show_query(query)              # Inspect SQL without executing it
collect(query)                 # Retrieve a tibble
collect_cached(query)          # Explicit local Parquet cache
collect_cached(query, refresh = TRUE)

lake_query("SELECT 1 AS example")
lake_query("SELECT 1 AS example", as = "arrow")
```

Printing a lazy table previews rows and executes a query. Empty results retain their schema. R output uses Arrow's conversions and consistently represents BIGINT as `bit64::integer64`; Arrow output preserves full precision and nested types. See `?ckdata::ckdata_types`. UNLOAD does not guarantee global row order: use `collect() |> arrange(...)` when local order matters.

For an explicit connection, use `DBI::dbConnect(ckdata::ckdata(), workgroup = "...", schema = "...", profile = "...")`, ordinary read-side DBI methods, and `DBI::dbDisconnect()`. Writes, transactions and remote materialization are unsupported. `median()` and `quantile()` require an explicit choice of `approx_median()` or `approx_quantile()` remotely. Additional assumed-role sessions expire without renewal. Call `lake_disconnect()`, then rebuild saved lazy queries from a fresh `lake_tbl()` to open a new managed session. Saved queries keep their old connection. For an explicit DBI connection, close and recreate it and rebuild its lazy queries.

For parallel work, use fresh R sessions or PSOCK workers and create connections in each worker. Forking after ckdata's native runtime starts cannot safely inherit its worker and is rejected.

The managed helpers retain the existing column-policy heuristics. They do not establish arbitrary SQL provenance or replace access controls. Cache entries are local data files, remain until removed by their owner, and do not detect source-data changes; use `refresh = TRUE` to update them.

Small local lookup frames can be joined with `left_join(..., copy = "inline")` or `dbplyr::copy_inline(con, lookup)`. dbplyr embeds the values into the SELECT query without creating a remote table. Inline double columns preserve signed infinity; NaN becomes SQL NULL, as in dbplyr projections. Keep inline inputs small.

Use positional `?` placeholders with `DBI::dbGetQuery(con, "SELECT ? AS value", params = list("O'Brien"))`, or prepare with `dbSendQuery()` and call `dbBind()` before fetching. Parameters use Athena's native execution API. An unnamed list binds by position; data frame columns also bind by position. Equal-length vectors execute one query per row and concatenate the results. Rebinding replaces the previous result. Zero-length vectors return a typed empty result using schema recovery. Bindings support logical, integer, double, character, factor, exact `integer64`, Date, POSIXt, difftime, and lists of raw vectors/NULL for binary values. Factor parameters are converted to character with a warning. Temporal predicates can bind difftime values as TIME; Athena UNLOAD cannot return TIME columns. Query timeouts apply to each execution.

Typed missing R literals in dbplyr projections retain their column types, including when the query has no rows. SQL still determines types in expressions such as `id + NA`. Raw SQL NULLs need an explicit cast when Athena cannot infer a type.

Raw SQL follows Parquet UNLOAD's output rules: give expressions column aliases (`SELECT 1 AS example`) and cast standalone NULL outputs (`SELECT CAST(NULL AS VARCHAR) AS example`). Athena also rejects duplicate output names and types unsupported by UNLOAD. ckdata does not rewrite SQL to change those outputs; its DBI support is a read-side subset rather than full conformance.

## Layout

| Path | What it holds |
|---|---|
| `core/` | The Rust core, the crate `ckdata` |
| `r/` | The R package, an extendr package. Its crate, `ckdata-r`, reaches the core through the symlink `r/src/rust/core`, which `R CMD build` copies into the source tarball |
| `python/` | The Python binding, the PyO3 crate `ckdata-py`, built with maturin into the module `ckdata` |

## Developing

Run these commands from the repository root. Install [Rust through rustup](https://rustup.rs), R 4.2 or later with devtools and rextendr, [uv](https://docs.astral.sh/uv/getting-started/installation/), and [Gitleaks](https://github.com/gitleaks/gitleaks#installing). Gitleaks must be on PATH: the system hook uses your installed executable. Python tests need Python 3.11 or later, which uv can provide.

```sh
uv tool install prek==0.5.9
prek install
```

The hooks check formatting on commits and run Clippy and all Rust targets on pushes, including dependency-only changes. CI repeats these checks. [Dependency maintenance](.github/dependency_policy.md) records update ownership and upstream constraints.

```sh
cargo test --workspace --all-targets --locked --exclude ckdata-py
```

```sh
Rscript -e 'devtools::test("r")'
```

Install development dependencies first with `Rscript -e 'pak::local_install_dev_deps("r")'`.

`Rscript -e 'devtools::document("r")'` regenerates R documentation. After changing an extendr export, run `cargo run --manifest-path rust/Cargo.toml --bin document` from `r/src`, then regenerate the R documentation.

The [query integration harness](tests/README.md) checks the Athena/Parquet result path against separately provisioned, entirely invented fixtures. The Rust acceptance example runs the implemented reader through both test roles. Its help and ordinary Rust tests are offline; live checks require an explicit flag and the deployment contract from ck-datalake's `environments/ckdata-test` root.

```sh
cargo run -p ckdata --example query_acceptance -- --help
cargo run -p ckdata --example query_acceptance -- \
  --deployment /absolute/path/to/ckdata-test-deployment.json --run-live
```

AWS profiles and region follow standard SDK configuration; `--profile` selects a source profile. The core's `Connection::query` accepts SQL, `QueryOptions` and a `QueryCancellation` token, and returns owned Arrow batches through `QueryResult`. Signal cancellation and keep awaiting the query so the client can request a remote stop. The library installs no global Ctrl-C handler.

The core wraps your SQL in UNLOAD after trimming whitespace and one trailing semicolon. Athena validates the SQL. Every successful `QueryResult` has a Parquet-derived `SchemaRef` and an Arrow reader, including empty results. If Athena writes no files, the core runs one schema-only UNLOAD, reads its Parquet schema and returns zero rows. Recovery shares the original timeout and cancellation signal; failures retain the original query ID and the recovery error. A zero-row Parquet file already has a schema and needs no recovery.

UNLOAD does not guarantee combined row order across files, even with `ORDER BY`. The core does not sort results; callers needing local order must sort after retrieval.

`into_reader_with_warnings()` returns the reader together with the query warnings. Hosts must present `SessionExpiring` warnings during the last fifteen minutes of an explicit role lease.

The repository has one Python development environment, declared in `pyproject.toml`
and resolved in `uv.lock`. It covers tooling, the AWS probe and Python binding
tests. uv creates the environment without an activation step.

```sh
uv sync --locked
uv run --locked pytest
```

This runs every Python tooling test under `tests/`, including the small R build
fixtures. Run `uv run --locked pytest tests/unit` for the fast checks without R
or a Rust build. [Testing](tests/README.md) describes the layout and live probe.

To build the Python binding and run its handoff tests in the same environment:

```sh
uv run --locked --group bindings --all-packages pytest python/tests
```

On Windows, git checks the core's symlink out as a text file unless symlinks are enabled. Turn on Developer Mode, then clone with `git clone -c core.symlinks=true https://github.com/coursekata/ckdata.git`.

## Releasing

Publish the first R release only after its query API works and its package tests pass. Publication is disabled by `R_RELEASE_READY` in the release workflow. Enable it in the reviewed first-release change, and add the package to the CourseKata R-universe registry with `subdir: r` and `branch: "*release"`. The tagged commit must also have successful main-branch CI.

1. In one pull request, set the new version in `core/Cargo.toml`, `python/Cargo.toml`, `r/src/rust/Cargo.toml` and `r/DESCRIPTION`, and merge it.
2. Tag the merge commit with a lightweight tag and push it: `git tag vX.Y.Z <commit> && git push origin vX.Y.Z`.
3. The release workflow checks that the tag matches all four versions and sits on `main`, then creates the GitHub release.
4. R-universe builds the R binaries from that release. The release workflow waits for them, then installs each on Linux, macOS and Windows with no Rust present and runs the package's tests. `verify_release` runs that check again by hand.

A release tag is never moved. A fix is a new version.

License notices ship with both packages. After changing a Cargo lock, run
`python3 scripts/check_licenses.py --write`; CI rejects stale notices. Before
building a release with a different Rust compiler, run
`python3 scripts/check_licenses.py --runtime-write --write` and review the
compiler provenance and license diff. `--archive /absolute/path/to/package`
checks the exact notices carried by a wheel, Python source archive or R tarball.
