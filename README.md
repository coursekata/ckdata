# ckdata

This repository will hold ckdata's shared core and its R and Python packages. Today it holds only a delivery spike, which tests one question: can a Rust core that links the AWS SDK for Rust and Apache Arrow reach R and Python users as prebuilt binaries, so that nobody installing it needs a Rust toolchain?

- `core/` links aws-config, aws-sdk-athena, aws-sdk-s3, and arrow-rs and parquet 59. It returns a literal table, round-tripped through Parquet: a NULL string, an empty string, a 64-bit integer past 2^53, and a naive timestamp. It makes no AWS call.
- `r/` is an extendr package. `ck_probe()` returns that table as a tibble through a nanoarrow stream. It depends on the core by Git, because R-universe builds it from a source tarball that a path dependency would point outside of.
- `python/` is a maturin package. `ckspike.probe()` returns an object that Polars, pyarrow, pandas and DuckDB each accept directly. `.github/workflows/wheels.yml` builds and checks the wheels.

Install the R binary from R-universe:

```r
install.packages("ckspike", repos = c("https://coursekata.r-universe.dev", "https://cloud.r-project.org"))
ckspike::ck_probe()
```

The spike's packages are named `ckspike` so they can't be mistaken for ckdata. They go away when the real core lands.
