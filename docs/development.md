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

Install [Just](https://just.systems/man/en/),
[Rust through rustup](https://rustup.rs), R 4.2 or later,
[uv](https://docs.astral.sh/uv/getting-started/installation/), Node.js/npm,
[Actionlint](https://github.com/rhysd/actionlint#installation) and
[Gitleaks](https://github.com/gitleaks/gitleaks#installing). Just 1.46 or later
is required for recipe flags; native builds require Rust 1.94.1 or later and
the platform prerequisites in the installation guide. These executables must
be on PATH. Just itself can be installed with `uv tool install rust-just`.

```sh
just setup
just
```

`just setup` installs rustfmt and Clippy, fetches locked Cargo dependencies,
prepares the Python workspace, installs the R package and development
dependencies with pak, and installs Lefthook's Git hooks. It is safe to rerun;
it does not authenticate to AWS or provision resources. The default R user
library is used; standard `R_LIBS_USER`/`R_LIBS` settings still apply.

Cargo outputs default to the shared root `target/`, outside the R source
package. Setup clears a legacy `r/src/rust/target/` cache if present. R copies
source directories before applying `.Rbuildignore`, so retaining that old cache
can exhaust temporary disk space during a source build. `build` fails early
with setup guidance if the legacy directory reappears. An explicit
`CARGO_TARGET_DIR` must also stay outside `r/`.

The uv development group pins Just, Lefthook, Ruff, Air and the standard file
checkers. `.github/package.json` and its npm lock own the existing Renovate
validator and its supported Node 24 runtime. Setup installs that runtime
locally and builds the validator's regex dependency with it. No global Node
switch or custom tool downloader is required.

| Command | Responsibility |
|---|---|
| `just fmt` | Apply rustfmt, Ruff and Air formatting, plus standard whitespace/end-of-file fixes |
| `just fmt --check` | Check formatting without modifying maintained source files |
| `just lint` | Run static analysis and repository checks without source changes |
| `just lint --fix` | Apply upstream automatic lint fixes and report remaining findings |
| `just test` | Run offline Rust, R, Python and repository-tooling tests |
| `just check` | Aggregate `fmt --check`, `lint` and `test` |
| `just build` | Build an R source archive and Python source/wheel distributions in `dist/` |
| `just build --check` | Build distributions, run `R CMD check` and test the installed Python wheel in a fresh environment |
| `just docs` | Regenerate the native R wrappers and R documentation |
| `just test-acceptance` | Run the explicitly configured synthetic AWS suite, with automatic login disabled |

`lint --target` accepts `rust`, `r`, `python` or `repo`; `test --target`
accepts `rust`, `r`, `python` or `tooling`. Both default to `all`.
Clippy's automatic fixes operate on the working tree, including files with
uncommitted changes; they do not stage changes. R lint findings require manual
changes. Generated bindings, upstream legal text and build outputs have the
appropriate formatting exclusions.

The justfile owns commands and their prerequisites. Lefthook owns Git events:
commits check formatting and scan staged file contents for secrets; pushes run
`just check`. All hooks use check mode. Existing prek-generated hooks are
uninstalled during setup; unrelated hooks are left to Lefthook's normal backup
handling. CI calls the same recipes where applicable and retains its platform
matrix, package archive checks and security inventories.

The repository is IDE agnostic. Editors may invoke the configured native
formatters on save; no editor settings or extension installation are provided.
[Dependency maintenance](../.github/dependency_policy.md) records update
ownership, scanners and upstream constraints.

## R

`just setup` installs the local package, its test dependencies and the tools
declared in `Config/Needs/development`. Regenerate native wrappers and R
documentation with:

```sh
just docs
```

This runs the extendr generator from `r/src`, then roxygen2 through devtools.
Run it after changing an extendr export or R function documentation.

`r/tools/config.R` configures the native build, including platform flags and
build-cache handling. Cargo enforces the manifests' minimum Rust version.

The two local review runbooks separate the
[internal researcher workflow](researcher-walkthrough.md) from
[QA acceptance](qa-walkthrough.md). Researcher installation and access setup
belong in CourseKata's internal documentation. The package provides function and
configuration references without provisioning a demo environment.

### Local UAT environment

The test operator prepares the local rehearsal before handing over the
researcher runbook. Install the current checkout using `just setup`, which
uses the normal R user library. The rehearsal must not require a separate
package library or a change to the researcher's library search path.

For the retained synthetic deployment, the prepared directory is
`.local/researcher-uat/`. Its ignored `.Renviron` selects the fixture-only AWS
configuration, source profile, region, restricted reader role, workgroup and
database. It also holds the synthetic acceptance contract path and the chosen
freshness schedule. Its `.Rprofile` clears conflicting inherited source settings
and sets rehearsal options without connecting. The legacy connection variables
are used here to exercise compatibility. No package-library variables are set.
Real deployment configuration should also select its database explicitly;
the package's generic database fallback is `default`.

Verify startup with an ordinary fresh R process whose initial working directory
is the prepared folder. For example, from the repository root:

```sh
cd .local/researcher-uat
R --no-save --no-restore
```

The same startup applies to a fresh R terminal launched there by an editor;
an RStudio project is optional. R must read its normal startup files, so do not
launch with `--vanilla`, `--no-environ` or `--no-init-file`. Changing a running
process's directory is not a new startup. Do not compensate by adding startup
loading calls to researcher code.

Before handoff, verify that `library(ckdata)` uses the ordinary user library,
that the fixture settings were loaded automatically and that no managed
connection exists yet. Startup and package loading must not authenticate.
Browser sign-in is exercised by the researcher's first connection. Separate
offline tests cover the sign-in handoff and failure paths without authenticating
the tester. The researcher runbook begins with package loading and ordinary
package calls; identity checks and deliberate failures belong in the QA runbook.

## Configuration boundaries and defaults

The R adapter resolves legacy R-package environment aliases in one mapper in
`r/R/settings.R`. Canonical `CKDATA_*` names win without rewriting the process's
`AWS_*` configuration. Both managed helpers and explicit DBI connections use
the same defaults. The shared Rust core receives explicit connection settings;
the thin Python adapter does not yet expose query configuration.

Runtime defaults select no CourseKata account, role, region, output bucket or
production database. A workgroup is required; the generic database fallback is
`default`. Credentials and region otherwise follow the AWS SDK chain. Result
locations use workgroup settings unless an allowed client override is supplied.
No freshness schedule or timezone is assumed. Saved results default to session
temporary storage; explicit snapshots default to the working directory.

The existing `lake_*` helper names and CourseKata column-migration policies are
retained for compatibility. Column checks are opt-in through audit/publish mode;
the default explore mode leaves queries alone. The core currently uses the
standard `AwsDataCatalog`; selecting other catalogs is an API capability gap,
not an organization-specific default. Release channels remain CourseKata-owned.

## Python

Python 3.11 or later is required; uv can provide it. The root `pyproject.toml`
and `uv.lock` define one development environment. Just invokes uv without an
environment activation step:

```sh
just setup
just test --target python
just test --target tooling
```

The `bindings` dependency group supplies maturin, PyArrow, dataframe libraries
and pytest. Setup and Python tests use uv's package-specific build setting to
install locked build tools before compiling the binding, including in a fresh
environment.
Distribution builds use the backend's isolated build path.

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
`into_reader()` transfers the Arrow reader. AWS providers manage credential
renewal; hosts can service `SsoLogin` requests without restarting the native
query future. The core does not sort results or promise an out-of-core memory
bound.

## Windows

Enable Developer Mode and clone with symlinks enabled; otherwise Git checks the
shared core's symlink out as a text file:

```sh
git clone -c core.symlinks=true https://github.com/coursekata/ckdata.git
```

Install matching Rtools and Rust's GNU target as described in the installation
guide.
