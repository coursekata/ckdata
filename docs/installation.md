# Installation and configuration

## Package availability

This repository has not published its first release. The R interface is
implemented locally; Python query execution and wheel publication are still
pending. The README describes the target interface for both languages. Its
Python query example is not runnable with the current development build.

Use [ckdata-r](https://github.com/coursekata/ckdata-r) for the published R package
until this replacement completes acceptance and its first binary release.
The release-channel commands below apply after publication; they do not select
this development build today.

## R

R 4.2 or later is required. The release channel is CourseKata's R-universe:

```r
install.packages("ckdata", repos = c(
  coursekata = "https://coursekata.r-universe.dev",
  CRAN = "https://cloud.r-project.org"
))
```

On macOS and Windows, use `type = "binary"` to require a binary and avoid a
source build. Linux binaries need a repository URL matching the distribution,
architecture and R minor version; see the
[R-universe binary installation instructions](https://docs.r-universe.dev/install/binaries.html).

The release checks target the current R release on Ubuntu 26.04 x86_64,
macOS arm64 and Windows x86_64. Binary availability must be verified against
the first release. Other systems or R versions may require a source build.

### Install the local source

Install [Rust through rustup](https://rustup.rs), version 1.94.1 or later, plus
a C compiler, make and xz. From the repository root:

```sh
Rscript -e 'install.packages("pak"); pak::local_install("r")'
```

On Windows, install the [Rtools version matching R](https://cran.r-project.org/bin/windows/Rtools/)
and add Rust's GNU target:

```sh
rustup target add x86_64-pc-windows-gnu
```

The checkout must preserve symlinks; see [Windows development](development.md#windows).

## Python

Python 3.11 or later is required. After wheel publication, install into your
project's Python environment:

```sh
python -m pip install ckdata pyarrow
```

PyArrow is used by the README example. Use pandas, Polars or DuckDB in the same
environment when working with those libraries. A compatible wheel avoids a
local Rust build. See Python's
[package installation guide](https://docs.python.org/3/installing/index.html)
for virtual environments and pip.

For the current native binding and its tests, use the repository's
[development environment](development.md#python).

## AWS access

ckdata uses standard AWS profiles, SSO credentials and region configuration.
In interactive R sessions, the first connection that needs SSO sign-in opens
AWS's browser login through AWS CLI v2 and retries the connection once. No
terminal sign-in command is needed. Install
[AWS CLI v2](https://docs.aws.amazon.com/cli/latest/userguide/getting-started-install.html)
and make `aws` available to R for this flow. Valid existing credentials and
hosted workload identities work without the CLI.

Use `DBI::dbConnect(ckdata::ckdata(), login = FALSE)` to fail immediately when
sign-in is needed, or set `options(ckdata.login = FALSE)` for managed helpers.
The connection argument overrides the option. Scripts and jobs default to this
fail-fast behavior; explicit `login = TRUE` permits browser sign-in. Other
authentication failures and permission denials propagate without opening login.
The Python query interface remains pending; this browser interaction is in R.

Set the source profile and lake settings in your application environment, using
values for your intended environment:

```sh
export AWS_PROFILE=your-profile
export AWS_REGION=your-region
export CK_DATALAKE_WORKGROUP=your-workgroup
export CK_DATALAKE_SCHEMA=your-database
```

The workgroup supplies the result location and encryption settings. There is
no separate staging-directory setting. `CK_DATALAKE_ROLE_ARN` optionally names
an additional role to assume once; see [session lifetime](usage.md#sessions-and-parallel-work).

In R, an explicit DBI connection can supply settings directly:

```r
con <- DBI::dbConnect(
  ckdata::ckdata(),
  profile = "your-profile",
  region = "your-region",
  workgroup = "your-workgroup",
  schema = "your-database",
  freshness = ckdata::daily_at("07:00", tz = "America/Los_Angeles")
)
```

Managed R helpers also accept `ckdata.workgroup` and `ckdata.schema` options.
For compatibility they accept `CK_DATALAKE_INTERNAL_PROFILE` and
`CK_DATALAKE_REGION`; explicit DBI connections use the standard AWS chain
unless corresponding arguments are supplied. See `?ckdata::ckdata_settings`
and `?ckdata::ckdata_options` for the R configuration reference.
