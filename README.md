# ckdata

ckdata reads the CourseKata data lake through Amazon Athena. This repository holds its Rust core and the R and Python packages built on it, which share one implementation of the connection, the query path and the Arrow handoff.

## Status

This is a pre-release. The packages build, install and pass their checks, but they run no query yet. To query the lake today, use [ckdata-r](https://github.com/coursekata/ckdata-r). The Python package is not released yet.

## Install

On macOS and Windows, R installs a binary from CourseKata's R-universe, so no Rust is needed:

```r
install.packages("ckdata", repos = c("https://coursekata.r-universe.dev", "https://cloud.r-project.org"))
```

On Linux, R-universe serves binaries from a separate repository for each distribution, architecture and R version, and builds them only for the Ubuntu release its [documentation](https://docs.r-universe.dev/install/binaries.html) names. This points R at the right one for the machine:

```r
codename <- sub("^VERSION_CODENAME=", "", grep("^VERSION_CODENAME=", readLines("/etc/os-release"), value = TRUE))
bin <- function(universe) sprintf("https://%s.r-universe.dev/bin/linux/%s-%s/%s/", universe, codename, R.version$arch, sub("\\.[0-9]+$", "", getRversion()))
install.packages("ckdata", repos = c(bin("coursekata"), bin("cran")))
```

Anywhere R-universe has no binary, `install.packages()` builds from source, which needs Rust.

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

```sh
uv venv --python 3.11 && source .venv/bin/activate
uvx maturin develop -m python/Cargo.toml
uv pip install --group python/pyproject.toml:test
pytest python/tests
```

On Windows, git checks the core's symlink out as a text file unless symlinks are enabled. Turn on Developer Mode, then clone with `git clone -c core.symlinks=true`.

## Releasing

1. In one pull request, set the new version in `core/Cargo.toml`, `python/Cargo.toml` and `r/DESCRIPTION`, and merge it.
2. Tag the merge commit with a lightweight tag and push it: `git tag vX.Y.Z <commit> && git push origin vX.Y.Z`.
3. The release workflow checks that the tag matches all three versions and sits on `main`, then creates the GitHub release.
4. R-universe builds the R binaries from that release. The release workflow waits for them, then installs each on Linux, macOS and Windows with no Rust present and runs the package's tests. `verify-release` runs that check again by hand.

A release tag is never moved. A fix is a new version.
