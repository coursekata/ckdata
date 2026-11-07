# Development

Run commands from the repository root unless stated otherwise. User installation
and AWS setup are in the [installation guide](installation.md).

## Repository structure

| Path | Responsibility |
|---|---|
| `core/` | Shared Rust Athena client, Parquet reader and Arrow results; crate `ckdata` |
| `r/` | R DBI/dbplyr package and extendr adapter; crate `ckdata-r` |
| `python/` | Python package and PyO3 adapter, built with maturin; crate `ckdata-py`, import `ckdata` |
| `tests/` | Tests of repository-owned tooling and instructions for package checks |
| `scripts/` | Repository maintenance tooling |

Language adapters handle their host interfaces and conversions; the core owns
the AWS query and result path. The R crate has its own workspace so its source
package builds independently. Its `r/src/rust/core` symlink reaches the shared
core; `R CMD build` copies that source into the package tarball.

## Tools and hooks

Install [Rust through rustup](https://rustup.rs), R 4.2 or later,
[uv](https://docs.astral.sh/uv/getting-started/installation/), and
[Gitleaks](https://github.com/gitleaks/gitleaks#installing). Native builds need
Rust 1.94.1 or later; see the source-install prerequisites in the installation
guide. Gitleaks must be on PATH for the system hook.

```sh
uv tool install prek==0.5.9
prek install
```

Hooks check formatting on commits. Push hooks run Clippy, Rust tests and Python
tests, including for dependency-only changes. CI repeats the checks.
[Dependency maintenance](../.github/dependency_policy.md) records update
ownership, scanners and upstream constraints.

## R

Install the local package and development dependencies:

```sh
Rscript -e 'install.packages("pak"); pak::local_install("r"); pak::local_install_dev_deps("r")'
```

Use devtools and rextendr for documentation and native interface generation.
Regenerate R documentation with:

```sh
Rscript -e 'devtools::document("r")'
```

After changing an extendr export, run
`cargo run --manifest-path rust/Cargo.toml --bin document` from `r/src`, then
regenerate R documentation.

`r/tools/config.R` configures the native build, including platform flags and
build-cache handling. Cargo enforces the manifests' minimum Rust version.

## Python

Python 3.11 or later is required; uv can provide it. The root `pyproject.toml`
and `uv.lock` define one development environment. uv runs commands in it without
an activation step:

```sh
uv sync --locked
uv run --locked pytest
```

The optional `bindings` group supplies maturin, PyArrow and dataframe libraries.
Build the binding and run its package tests with:

```sh
uv run --locked --group bindings --all-packages pytest python/tests
```

## Testing

Follow [Testing](../tests/README.md) for Cargo, R package and Python checks, plus
the opt-in synthetic AWS suites. Each package uses its normal test runner.
Live checks use separately provisioned invented fixtures and restricted roles.

## Shared Rust query API

`Connection::query` takes SQL, `QueryOptions` and a `QueryCancellation` token.
It returns a `QueryResult` with a Parquet-derived schema and owned Arrow batches.
The core trims whitespace and one trailing semicolon before wrapping the query
in UNLOAD. Athena validates the SQL. The SDK's paginated manifest API supplies
file paths, and the upstream Parquet reader decodes the files.

Every successful result includes a schema and reader. Fileless results use one
schema-only UNLOAD; a zero-row Parquet file needs no recovery. Recovery shares
the original timeout and cancellation signal. Failures retain the original
query ID and the recovery error.

Signal cancellation and keep awaiting the query so the client can request a
remote stop. The core installs no global Ctrl-C handler.
`into_reader_with_warnings()` returns query warnings with the reader; hosts
must present `SessionExpiring` warnings during the last fifteen minutes of an
explicit role lease. The core does not sort results or promise an out-of-core
memory bound.

## Windows

Enable Developer Mode and clone with symlinks enabled; otherwise Git checks the
shared core's symlink out as a text file:

```sh
git clone -c core.symlinks=true https://github.com/coursekata/ckdata.git
```

Install matching Rtools and Rust's GNU target as described in the installation
guide.
