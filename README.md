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

Run these commands from the repository root. Install [Rust through rustup](https://rustup.rs), R 4.2 or later with devtools and rextendr, [uv](https://docs.astral.sh/uv/getting-started/installation/), and [Gitleaks](https://github.com/gitleaks/gitleaks#installing). Gitleaks must be on PATH: the system hook uses your installed executable. Python tests need Python 3.11 or later, which uv can provide.

```sh
uv tool install prek==0.5.9
prek install
```

The hooks check formatting on commits and run Clippy and all Rust targets on pushes, including dependency-only changes. CI repeats these checks. [Dependency maintenance](.github/dependency-policy.md) records update ownership and upstream constraints.

```sh
cargo test --workspace --all-targets --locked --exclude ckdata-py
```

```sh
Rscript -e 'devtools::test("r")'
```

`Rscript -e 'devtools::document("r")'` regenerates R documentation and wrappers after an exported function changes.

The [query integration harness](scripts/query-integration/README.md) checks the Athena/Parquet result path against separately provisioned, entirely invented fixtures. The Rust acceptance example runs the implemented reader through both test roles. Its help and ordinary Rust tests are offline; live checks require an explicit flag and the deployment contract from ck-datalake's `environments/ckdata-test` root.

```sh
cargo run -p ckdata --example query-acceptance -- --help
cargo run -p ckdata --example query-acceptance -- \
  --deployment /absolute/path/to/ckdata-test-deployment.json --run-live
```

AWS profiles and region follow standard SDK configuration; `--profile` selects a source profile. The core's `Connection::query` accepts SQL, `QueryOptions` and a `QueryCancellation` token, and returns owned Arrow batches through `QueryResult`. Signal cancellation and keep awaiting the query so the client can request a remote stop. The library installs no global Ctrl-C handler.

```sh
uv venv --python 3.11
source .venv/bin/activate
uv pip install --group python/pyproject.toml:test 'maturin>=1.15,<2'
maturin develop --manifest-path python/Cargo.toml --locked
pytest python/tests
```

On PowerShell, activate with `.venv\Scripts\Activate.ps1` instead of `source`. The other commands use the same repository-root paths.

On Windows, git checks the core's symlink out as a text file unless symlinks are enabled. Turn on Developer Mode, then clone with `git clone -c core.symlinks=true`.

## Releasing

Publish the first R release only after its query API works and its package tests pass. Publication is disabled by `R_RELEASE_READY` in the release workflow. Enable it in the reviewed first-release change, and add the package to the CourseKata R-universe registry with `subdir: r` and `branch: "*release"`. The tagged commit must also have successful main-branch CI.

1. In one pull request, set the new version in `core/Cargo.toml`, `python/Cargo.toml`, `r/src/rust/Cargo.toml` and `r/DESCRIPTION`, and merge it.
2. Tag the merge commit with a lightweight tag and push it: `git tag vX.Y.Z <commit> && git push origin vX.Y.Z`.
3. The release workflow checks that the tag matches all four versions and sits on `main`, then creates the GitHub release.
4. R-universe builds the R binaries from that release. The release workflow waits for them, then installs each on Linux, macOS and Windows with no Rust present and runs the package's tests. `verify-release` runs that check again by hand.

A release tag is never moved. A fix is a new version.

License notices ship with both packages. After changing a Cargo lock, run
`python3 scripts/check-licenses.py --write`; CI rejects stale notices. Before
building a release with a different Rust compiler, run
`python3 scripts/check-licenses.py --runtime-write --write` and review the
compiler provenance and license diff. `--archive /absolute/path/to/package`
checks the exact notices carried by a wheel, Python source archive or R tarball.
