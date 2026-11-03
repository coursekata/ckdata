# ckdata

ckdata is being rebuilt around a Rust core shared by its R and Python packages. The core connects to Athena, reads Parquet results into Arrow and restores supported final ordering. The R and Python query bindings are still in development.

## Status

This is a development build. The packages build, install and pass their checks, but they run no query yet and have not been published. To query the lake today, use [ckdata-r](https://github.com/coursekata/ckdata-r). The first public R release will follow a working query API and passing package tests. Python wheels and PyPI publication follow full Python package testing.

The Rust core supports standard AWS connection settings, a fixed-lifetime assumed role and table/column discovery through Athena's metadata APIs. Queries use UNLOAD with a fresh workgroup output prefix, bounded manifest/Parquet reads, typed empty results and cancellation. Supported outer `ORDER BY` keys are sorted across files; unsupported keys produce a warning. Tests use invented configuration and mocked HTTP services.

## Install

For the working R package, follow the installation instructions in [ckdata-r](https://github.com/coursekata/ckdata-r). This rewrite is not available from CourseKata's R-universe. Its binary-install instructions will be added with the first usable R release.

## Layout

| Path | What it holds |
|---|---|
| `core/` | The Rust core, the crate `ckdata` |
| `r/` | The R package, an extendr package. Its crate, `ckdata-r`, reaches the core through the symlink `r/src/rust/core`, which `R CMD build` copies into the source tarball |
| `python/` | The Python binding, the PyO3 crate `ckdata-py`, built with maturin into the module `ckdata` |

## Developing

You need Rust stable, R 4.2 or later with devtools and rextendr, and, for the Python tests, Python 3.11 or later with uv. Run `prek install` once: the hooks check formatting on every commit, and clippy and the Rust tests on every push.

```sh
cargo test --workspace --exclude ckdata-py
```

```sh
cd r && Rscript -e 'devtools::test()'
```

`devtools::document()`, run from `r/`, regenerates the R wrappers after a change to the R crate's exported functions.

The [query integration harness](scripts/query-integration/README.md) checks the Athena/Parquet result path against separately provisioned, entirely invented fixtures. The Rust acceptance example runs the implemented reader through both test roles. Its help and ordinary Rust tests are offline; live checks require an explicit flag and the deployment contract from ck-datalake's `environments/ckdata-test` root.

```sh
cargo run -p ckdata --example query-acceptance -- --help
cargo run -p ckdata --example query-acceptance -- \
  --deployment /absolute/path/to/ckdata-test-deployment.json --run-live
```

AWS profiles and region follow standard SDK configuration; `--profile` selects a source profile. The core's `Connection::query` accepts SQL, `QueryOptions` and a `QueryCancellation` token, and returns owned Arrow batches through `QueryResult`. Signal cancellation and keep awaiting the query so the client can request a remote stop. The library installs no global Ctrl-C handler.

```sh
uv venv --python 3.11 && source .venv/bin/activate
uvx maturin develop -m python/Cargo.toml
uv pip install --group python/pyproject.toml:test
pytest python/tests
```

On Windows, git checks the core's symlink out as a text file unless symlinks are enabled. Turn on Developer Mode, then clone with `git clone -c core.symlinks=true`.

## Releasing

Publish the first R release only after its query API works and its package tests pass. At that point, add the package to the CourseKata R-universe registry with `subdir: r` and `branch: "*release"`. The current development stub is not published.

1. In one pull request, set the new version in `core/Cargo.toml`, `python/Cargo.toml` and `r/DESCRIPTION`, and merge it.
2. Tag the merge commit with a lightweight tag and push it: `git tag vX.Y.Z <commit> && git push origin vX.Y.Z`.
3. The release workflow checks that the tag matches all three versions and sits on `main`, then creates the GitHub release.
4. R-universe builds the R binaries from that release. The release workflow waits for them, then installs each on Linux, macOS and Windows with no Rust present and runs the package's tests. `verify-release` runs that check again by hand.

A release tag is never moved. A fix is a new version.
