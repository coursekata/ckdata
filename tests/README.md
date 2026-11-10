# Tests

Tests stay with the code they exercise. The shared Rust core uses Cargo, the R
package uses testthat, and the Python binding uses pytest. Root `tests/unit/`
contains tests of repository-owned release and license tooling; it is not the
Python package's test suite.

## Package checks

Run these commands from the repository root:

```sh
just setup
just check
just build --check
```

`check` aggregates formatting checks, static analysis and offline tests. To run
only tests, use `just test`, or select `--target rust`, `--target r`,
`--target python` or `--target tooling`. The recipes use Cargo, testthat and
pytest directly. `build --check` validates the built R source archive and tests
the installed Python wheel in a temporary environment; it publishes nothing.

CI builds the actual R source tarball, then runs `R CMD check` against it on Linux,
macOS and Windows as soon as that archive is ready. Rust lint, core tests and
Python binding tests run in separate jobs. The final gate requires every check;
native caches are saved after failed checks so later runs can reuse completed
builds. Both Rust workspaces use thin LTO for release builds and leave parallel
code generation at Cargo's default. The release workflow installs R-universe
binaries without Rust and runs testthat against the installed package. There is no
separate R build simulator or tarball checker.

The R crate has its own workspace so the source package builds independently of
this checkout. The R test recipe runs its native tests separately from the root
Cargo workspace.
`uv.lock` describes one Python development environment; the optional `bindings`
group supplies maturin, PyArrow and the frame libraries. Python query behavior
will be tested under `python/tests/` as that interface is implemented.

## R tests

`r/tests/testthat/` covers DBI cursor lifecycle and metadata, quoting and binding,
dbplyr translations, column policy, saved results, Arrow/R conversions,
interrupts, authentication precedence, bounded SSO sign-in and credential
renewal. Ordinary tests use invented native Arrow fixtures and SQLite tables.
Selected DBItest cases exercise the read-side contract; writes and
transactions are outside this backend's scope.

Private native `test_support` modules construct the offline binding fixtures. R's
small native fixture entrypoints stay in the installed library so testthat can
exercise the real result cursors, conversion, stream ownership and asynchronous
jobs. The shared core has no public fixture API; its tests cover actual Parquet
retrieval through fake AWS responses.

### Live synthetic tests

The `test_live_*.R` files are ordinary testthat tests. The query cases skip before
credential discovery unless `CKDATA_RUN_LIVE_TESTS` is exactly `true`. Once enabled,
missing configuration, authentication failures and fixture drift fail the tests.
Offline gate tests check that behavior without contacting AWS.

Install the local R package and its test dependencies first. From an R session
at the repository root, run:

```r
withr::with_envvar(c(
  CKDATA_RUN_LIVE_TESTS = "true",
  CKDATA_TEST_DEPLOYMENT = "/absolute/path/to/ckdata-test-deployment.json",
  CKDATA_TEST_SEAT = "internal"
), {
  testthat::test_dir(
    "r/tests/testthat", filter = "^live_", package = "ckdata",
    load_package = "installed", stop_on_failure = TRUE
  )
})
```

Run again with `CKDATA_TEST_SEAT = "research"` and the appropriate standard AWS
source identity. The internal source must be able to assume its synthetic reader;
the research source is the fixture's restricted source role. Each test assumes
only its selected reader and verifies account, role, workgroup, output protection
and schema before querying the invented `types` table or fixed SQL values.
The fixture restores options, environment and managed connections after each test.
It never deletes AWS outputs or reads real research tables.

Coverage includes scalar and nested values, populated/empty schema equality,
wide integers and decimals, temporal ranges, binary values, Unicode, DBI paging,
native parameter binding, dbplyr execution, inline lookup joins and saved Parquet
results. Comparisons do not assume cross-file UNLOAD ordering.

The deployment contract comes from ck-datalake's independent
`environments/ckdata-test` environment. Its infrastructure tests own the fixture
permission policies and resource lifecycle. Real-role/enclave checks and actual
one-hour expiry remain final BB-6313 acceptance; any real-data query requires
Adam's prior approval and must use aggregates without PII.

## Rust live checks

The native reader's existing acceptance example uses the same invented fixture:

```sh
cargo run -p ckdata --example query_acceptance -- --help
cargo run -p ckdata --example query_acceptance -- \
  --deployment /absolute/path/to/ckdata-test-deployment.json --run-live
```

The contract is validated before authentication. The example assumes the restricted
test roles, checks their identities and workgroup settings, and tests the real
reader without printing rows or deleting outputs. No arbitrary SQL option exists.
Role renewal is covered offline with a virtual clock and synthetic AWS responses.

The former independent Python AWS probe has been retired. Live package tests use
the implemented reader instead of maintaining a second query/manifest decoder.
