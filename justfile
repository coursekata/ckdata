set shell := ["bash", "-euo", "pipefail", "-c"]
set windows-shell := ["bash", "-euo", "pipefail", "-c"]

export CARGO_TARGET_DIR := env("CARGO_TARGET_DIR", justfile_directory() / "target")

# Show the repository's developer commands.
[default]
_default:
    @just --list

# Prepare development dependencies and install Git hooks; no AWS access.
setup:
    @for tool in rustup Rscript node npm actionlint gitleaks jq; do command -v "$tool" >/dev/null || { echo "Install $tool first; see docs/development.md" >&2; exit 1; }; done
    if [ -d r/src/rust/target ]; then cargo clean --manifest-path r/src/rust/Cargo.toml --target-dir r/src/rust/target; fi
    rustup component add rustfmt clippy
    uv sync --locked --group bindings --extra ibis --no-build-isolation-package ckdata
    cargo fetch --locked
    cargo fetch --locked --manifest-path r/src/rust/Cargo.toml
    just _setup-repository-tools
    just _setup-r
    just _install-hooks

# Format maintained sources; --check reports differences without editing them.
[arg("check", long, value="true")]
fmt check="false": (_fmt-just check) (_fmt-rust check) (_fmt-python check) (_fmt-r check) (_fmt-files check)

# Static analysis; --fix applies supported fixes. --target selects a language or repo tooling.
[arg("fix", long, value="true")]
[arg("target", long, pattern="all|rust|python|r|repo")]
lint fix="false" target="all":
    just _lint-{{ target }} {{ fix }}

# Run offline tests; --target selects rust, r, python or tooling.
[arg("target", long, pattern="all|rust|r|python|tooling")]
test target="all":
    just _test-{{ target }}

# Aggregate local validation: formatting, static analysis and offline tests.
check: (fmt "true") lint test

# Build R/Python distributions; --check validates the resulting packages locally.
[arg("check", long, value="true")]
build check="false":
    @if [ -d r/src/rust/target ]; then echo 'A Rust build cache remains inside r/. Run just setup to clear that generated cache; Cargo outputs must stay outside the R source package.' >&2; exit 1; fi
    mkdir -p dist
    (cd dist && R CMD build ../r)
    uv build --python "$(uv run --locked --only-group dev python -c 'import sys; print(sys.executable)')" --out-dir dist
    if [ '{{ check }}' = true ]; then just _check-packages; fi

# Regenerate native R bindings and R documentation.
docs:
    (cd r/src && Rscript --vanilla -e 'Sys.setenv(R_INCLUDE_DIR = R.home("include")); quit(status = system2("cargo", c("run", "--locked", "--manifest-path", "rust/Cargo.toml", "--bin", "document")))')
    Rscript --vanilla -e 'devtools::document("r")'

# Refresh dependency license reports after changing either Cargo lockfile.
licenses:
    cargo about generate --locked --fail --manifest-path Cargo.toml --workspace --config about.toml about.hbs --output-file python/THIRD_PARTY_LICENSES.txt
    cargo about generate --locked --fail --manifest-path r/src/rust/Cargo.toml --config about.toml about.hbs --output-file r/inst/THIRD_PARTY_LICENSES.txt

# Run the opt-in synthetic acceptance suite; requires an explicit deployment contract and seat.
test-acceptance:
    @test -n "${CKDATA_TEST_DEPLOYMENT:-}" && test -n "${CKDATA_TEST_SEAT:-}" && test -n "${CKDATA_TEST_PROFILE:-}" || { echo 'Set CKDATA_TEST_DEPLOYMENT, CKDATA_TEST_SEAT and CKDATA_TEST_PROFILE for the synthetic fixture.' >&2; exit 1; }
    Rscript --vanilla -e 'options(ckdata.login = FALSE); Sys.setenv(CKDATA_RUN_LIVE_TESTS = "true"); testthat::test_dir("r/tests/testthat", filter = "^live_", package = "ckdata", load_package = "installed", stop_on_failure = TRUE)'
    CKDATA_RUN_LIVE_TESTS=true uv run --locked --no-build-isolation-package ckdata --group bindings --extra ibis pytest python/tests/test_live.py

_setup-r:
    Rscript --vanilla -e 'if (!nzchar(Sys.getenv("R_LIBS"))) { dir.create(Sys.getenv("R_LIBS_USER"), recursive = TRUE, showWarnings = FALSE); .libPaths(c(Sys.getenv("R_LIBS_USER"), .libPaths())) }; if (!requireNamespace("pak", quietly = TRUE)) install.packages("pak", repos = "https://cloud.r-project.org"); pak::local_install_dev_deps("r", upgrade = FALSE, ask = FALSE, dependencies = c("all", "Config/Needs/development")); stopifnot(all(vapply(c("devtools", "lintr", "rcmdcheck", "rextendr", "roxygen2"), requireNamespace, logical(1), quietly = TRUE)))'
    R CMD INSTALL r

_setup-repository-tools:
    npm ci --ignore-scripts --prefix .github
    npm rebuild node --prefix .github
    PATH="$PWD/.github/node_modules/node/bin:$PATH" npm rebuild re2 --prefix .github

_install-hooks:
    if command -v prek >/dev/null; then prek uninstall --hook-type pre-commit --hook-type pre-push; fi
    uv run --locked --only-group dev lefthook install

_fmt-rust check:
    cargo fmt --all {{ if check == "true" { "--check" } else { "" } }}
    cargo fmt --all --manifest-path r/src/rust/Cargo.toml {{ if check == "true" { "--check" } else { "" } }}

_fmt-just check:
    uv run --locked --only-group dev just --fmt {{ if check == "true" { "--check" } else { "" } }}

_fmt-python check:
    uv run --locked --only-group dev ruff format {{ if check == "true" { "--check" } else { "" } }} scripts tests python

_fmt-r check:
    uv run --locked --only-group dev air format {{ if check == "true" { "--check" } else { "" } }} r

_fmt-files check:
    uv run --locked --only-group dev python scripts/repo-files.py style {{ if check == "true" { "" } else { "--fix" } }}

_lint-all fix: (_lint-repo fix) (_lint-python fix) (_lint-r fix) (_lint-rust fix)

_lint-repo fix:
    uv run --locked --only-group dev python scripts/repo-files.py lint
    actionlint .github/workflows/*.yml
    npm run --prefix .github validate

_lint-python fix:
    uv run --locked --only-group dev ruff check {{ if fix == "true" { "--fix --no-unsafe-fixes" } else { "" } }} scripts tests python

_lint-r fix:
    # lintr reports findings; it has no general automatic fix mode.
    Rscript --vanilla -e 'paths <- jsonlite::fromJSON(system2("uv", c("run", "--locked", "--only-group", "dev", "python", "scripts/repo-files.py", "r-files"), stdout = TRUE)); findings <- unlist(lapply(paths, lintr::lint), recursive = FALSE); print(findings); quit(status = as.integer(length(findings) > 0L))'

_lint-rust fix:
    cargo clippy --workspace --all-targets --locked {{ if fix == "true" { "--fix --allow-dirty --allow-staged" } else { "" } }} -- -D warnings
    Rscript --vanilla -e 'Sys.setenv(R_INCLUDE_DIR = R.home("include")); quit(status = system2("cargo", c("clippy", "--manifest-path", "r/src/rust/Cargo.toml", "--all-targets", "--locked", {{ if fix == "true" { "\"--fix\", \"--allow-dirty\", \"--allow-staged\"," } else { "" } }} "--", "-D", "warnings")))'

_test-all: _test-rust _test-r _test-python _test-tooling

_test-rust:
    cargo test --workspace --all-targets --locked --exclude ckdata-py

_test-r:
    Rscript --vanilla -e 'Sys.setenv(R_INCLUDE_DIR = R.home("include"), DEBUG = "true", CKDATA_RUN_LIVE_TESTS = "false"); status <- system2("cargo", c("test", "--manifest-path", "r/src/rust/Cargo.toml", "--all-targets", "--locked")); if (status != 0L) quit(status = status); devtools::test("r", stop_on_failure = TRUE)'

_test-python:
    uv run --locked --no-build-isolation-package ckdata --group bindings --extra ibis pytest python/tests

_test-tooling:
    uv run --locked --only-group dev pytest tests

_check-packages:
    Rscript --vanilla -e 'Sys.setenv(CKDATA_RUN_LIVE_TESTS = "false"); tarball <- list.files("dist", pattern = "[.]tar[.]gz$", full.names = TRUE); tarball <- tarball[grepl("/ckdata_", tarball)]; stopifnot(length(tarball) == 1L); rcmdcheck::rcmdcheck(tarball, args = "--no-manual", error_on = "warning", check_dir = "dist/r-check")'
    just _check-wheel

[positional-arguments]
[script("uv", "run", "--locked", "--only-group", "dev", "python")]
_check-wheel python="":
    import os
    import pathlib
    import subprocess
    import sys
    import tempfile

    def run(*args):
        subprocess.run(args, check=True)

    wheels = list(pathlib.Path("dist").glob("ckdata-*.whl"))
    if len(wheels) != 1:
        raise SystemExit("Expected one ckdata wheel in dist; remove obsolete ckdata artifacts first.")
    with tempfile.TemporaryDirectory(prefix="ckdata-wheel-") as temporary:
        environment = pathlib.Path(temporary) / "venv"
        requirements = pathlib.Path(temporary) / "requirements.txt"
        run("uv", "export", "--locked", "--no-dev", "--group", "bindings", "--extra", "ibis", "--no-emit-project",
            "--no-hashes", "--output-file", str(requirements))
        run("uv", "venv", "--python", sys.argv[1] or sys.executable, str(environment))
        python = environment / ("Scripts/python.exe" if os.name == "nt" else "bin/python")
        run("uv", "pip", "install", "--python", str(python), "-r", str(requirements), str(wheels[0]))
        run(str(python), "-I", "-m", "pytest", "-c", "pyproject.toml", "python/tests")
