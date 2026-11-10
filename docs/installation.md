# Installation and configuration

## Package availability

The R installation commands below use tagged releases from CourseKata's
R-universe. A new release becomes installable once its binary builds finish.

Python query execution and wheel publication are still pending. The README's
Python example describes the target interface and is not runnable with the
current development build.

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
python -m pip install ckdata pyarrow pandas
```

The README example uses PyArrow and pandas. Install Polars or DuckDB in the same
environment when working with those libraries. A compatible wheel avoids a
local Rust build. See Python's
[package installation guide](https://docs.python.org/3/installing/index.html)
for virtual environments and pip.

For pandas, `result.to_pandas(types_mapper=pd.ArrowDtype)` preserves Arrow types,
including nullable 64-bit integers. Default pandas conversion can convert those
integers to floating point and lose precision.

For the current native binding and its tests, use the repository's
[development environment](development.md#python).

## AWS access

CourseKata researcher access and environment provisioning are documented
internally. This repository records package installation and configuration;
it does not provision an example environment. The package's runtime settings
remain generic and accept the previous R package's environment names.

ckdata uses standard AWS profiles, SSO credentials and region configuration.
In interactive R sessions, the first connection that needs SSO sign-in opens
AWS's browser login through AWS CLI v2 and resumes the same operation. No
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

Set the source profile and connection settings in your application environment, using
values for your intended environment:

```sh
export AWS_PROFILE=your-profile
export AWS_REGION=your-region
export CKDATA_WORKGROUP=your-workgroup
export CKDATA_SCHEMA=your-database
```

The workgroup supplies the result location and encryption settings by default.
`CKDATA_OUTPUT_LOCATION` is an optional S3 result prefix when the workgroup permits
client settings. `CKDATA_ROLE_ARN` optionally requires an additional role, using
AWS-managed renewal; see [session lifetime](usage.md#sessions-and-parallel-work).

The old R package's connection variables remain supported:

| Canonical name | Legacy alias |
|---|---|
| `CKDATA_PROFILE` | `CK_DATALAKE_INTERNAL_PROFILE` |
| `CKDATA_REGION` | `CK_DATALAKE_REGION` |
| `CKDATA_WORKGROUP` | `CK_DATALAKE_WORKGROUP` |
| `CKDATA_SCHEMA` | `CK_DATALAKE_SCHEMA` |
| `CKDATA_ROLE_ARN` | `CK_DATALAKE_ROLE_ARN` |
| `CKDATA_OUTPUT_LOCATION` | `CK_DATALAKE_S3_STAGING_DIR` |

A nonempty canonical variable wins. Empty and absent variables both fall back
to their legacy aliases on every platform. To bypass an inherited optional
setting, pass an explicit `NULL` connection argument, such as `role_arn = NULL`.
No AWS environment variable is rewritten.
`CKDATA_PROFILE` and `CKDATA_REGION` are optional package-specific overrides;
without them, standard AWS discovery applies.

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
The schema falls back to `default`; set it explicitly for your source. No
organization-specific database is selected automatically.
All six aliases apply to both managed helpers and explicit DBI connections.
Connection arguments override environment configuration; explicit NULL bypasses
optional profile, region, role and result-location settings. Existing R options
for workgroup and schema take precedence over their environment variables.

Users who previously relied on the old package's implicit CourseKata marts
database must set `CKDATA_SCHEMA` or `CK_DATALAKE_SCHEMA` explicitly. Compatibility
preserves named settings; it does not restore an organization-specific default.
See `?ckdata::ckdata_settings` and `?ckdata::ckdata_options` for the reference.
