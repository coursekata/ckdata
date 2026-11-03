# Dependency maintenance

The repository owner listed in [CODEOWNERS](CODEOWNERS) reviews dependency findings. Review the complete inventory on dependency pull requests, before a release, and during the monthly script-lock refresh. Recheck the deferred items below by 2026-12-01, or sooner when a parent package releases, an advisory changes, or the affected feature becomes reachable from untrusted input.

The shared quality workflow gates fixed HIGH/CRITICAL findings. The repository inventory retains UNKNOWN, LOW and MEDIUM findings, unfixed advisories, and development dependencies. OSV also reports Rust maintenance advisories that Trivy's vulnerability database may omit. No severity or fix filter belongs in the inventory. A clean Trivy result alone is insufficient to close a maintenance finding.

Run `scripts/query-integration/audit-advisories.py --output <report.json>` alongside the Trivy inventory. It reads only `Cargo.lock`, `r/src/rust/Cargo.lock`, and `scripts/query-integration/check.py.lock`; it sends public registry package names and versions to [OSV](https://google.github.io/osv.dev/post-v1-querybatch/). Its report preserves the full advisory records and exits unsuccessfully if the remote response is incomplete. Retain the JSON as a CI artifact and append the summary to the job summary. An unavailable advisory service is a failed inventory, not a clean result.

The Renovate validator also has an npm dependency graph even though this repository has no npm application. After the configured hook environment has been prepared, capture its existing graph with `npm ls --all --json` and pass the JSON through `--npm-tree`. The helper queries [npm's bulk advisory endpoint](https://docs.npmjs.com/cli/v11/commands/npm-audit/#bulk-advisory-endpoint) for every installed version. Keep `npm ls` tree errors in the report. Missing development dependencies in the published tool do not authorize an install or a cleanliness claim. This is an installed-version advisory inventory; npm's metavulnerability totals are not calculated. A run without `--npm-tree` explicitly reports that Node tooling was not audited.

A finding is closed when the dependency no longer resolves to an affected version, with a fresh audit attached. For a finding that cannot yet be fixed, record the exact advisory, package, parent constraint, owner and next review trigger here. Keep it visible in reports. Do not add an ignore rule, widen an upstream dependency constraint, edit a generated hook environment, or use an unreviewed fork to make a report pass. New vulnerability findings need an explicit risk decision by the repository owner; the entries below do not accept other advisories or future versions automatically.

## Current findings requiring follow-up

Reviewed 2026-11-02. All entries remain visible in the complete inventory.

| Finding | Scope and present constraint | Action and review trigger |
|---|---|---|
| [RUSTSEC-2024-0436](https://rustsec.org/advisories/RUSTSEC-2024-0436.html), informational/unmaintained | `paste 1.0.15`, required by the current `extendr-api 0.9.0`. No patched paste release. This advisory reports maintenance status and assigns no disclosed vulnerability or CVSS score. | Monitor extendr for a release that removes/replaces paste. Reevaluate the R binding dependency if a security advisory appears, the replacement is released, or maintenance stops blocking an upgrade. Review monthly; do not fork extendr solely to silence this informational finding. |
| [GHSA-3g3m-2fjr-6pw4](https://github.com/advisories/GHSA-3g3m-2fjr-6pw4), moderate | Renovate `44.206.2` pins affected `glob 13.0.6`; `glob 14.0.2` is the first fix. Existing transitive copies must be checked independently. | Update the hook when a tested Renovate release adopts the fix; audit the installed graph after every hook update. |
| [GHSA-g3hv-75wg-m249](https://github.com/advisories/GHSA-g3hv-75wg-m249), moderate | Renovate `44.206.2` pins affected `minimatch 10.2.6`; `minimatch 11.0.1` is the first fix. | Same review trigger. Retain every installed affected version and dependency path in audit evidence. |
| [GHSA-gcqx-w48m-w2h4](https://github.com/advisories/GHSA-gcqx-w48m-w2h4), low | Renovate `44.206.2` pins `deepmerge 4.3.1`, and no fixed release exists. | Monitor deepmerge and Renovate releases monthly and when the advisory changes. Reevaluate use of the validator if the affected merge operation is found to process attacker-controlled configuration. |

These npm findings concern the developer validator. Published [Renovate 44.206.2 metadata](https://registry.npmjs.org/renovate/44.206.2) still pins all three affected direct versions. Updating the hook alone does not close them. No repository npm overrides or cache edits are maintained.

## Updating locks and independent environments

Update both Cargo locks in one change and review their dependency differences. The root lock previously lagged `cc 1.11.0`, `crc32fast 1.5.4`, and `ctutils 0.4.8`; the R lock already contained those versions. Targeted commands are:

```sh
for manifest in Cargo.toml r/src/rust/Cargo.toml; do
  cargo update --manifest-path "$manifest" -p cc --precise 1.11.0
  cargo update --manifest-path "$manifest" -p crc32fast --precise 1.5.4
  cargo update --manifest-path "$manifest" -p ctutils --precise 0.4.8
done
```

`generic-array 0.14.9` looks compatible by its own version range, but `crypto-common 0.1.7` requires `generic-array =0.14.7` exactly. A targeted update to 0.14.9 fails in both workspaces. Retain 0.14.7 until a parent release changes that requirement; do not hand-edit either lock. The 1.4.5 line is a separate breaking upgrade.

The seven-day wait is configured in the script metadata so locked execution uses the same resolver policy. The query harness runs independently of the binding. Its PyArrow 26 upgrade uses `pyarrow>=26,<27`, supports the harness's declared Python 3.11 minimum, and passes the offline synthetic self-test. Refresh and test its lock with:

```sh
uv lock --script scripts/query-integration/check.py --upgrade --exclude-newer '7 days' --no-build
AWS_CONFIG_FILE=/dev/null AWS_SHARED_CREDENTIALS_FILE=/dev/null AWS_EC2_METADATA_DISABLED=true \
  uv run --locked scripts/query-integration/check.py --self-test
```

`python-dateutil 4.0.0` remains blocked by botocore's `python-dateutil>=2.1,<3.0.0` requirement. Keep the latest allowed 2.9.0.post0 until a compatible botocore release changes it. boto3/botocore and the other script-lock packages were current at this review. Renovate's PEP 723 manager tracks the inline requirements; the monthly script-lock workflow refreshes their resolution.

The Python build/test and R package environments are unpinned. A lock audit does not establish their installed transitive graph. Before a release, retain the versions from the actual tested environments (`python -m pip freeze` and R `sessionInfo()`), audit that Python graph, and include R dependency versions in the check evidence. Review CRAN versions for bit64, nanoarrow, tibble and testthat monthly; Renovate has no native CRAN manager. `RoxygenNote` records the generator used, rather than imposing an install constraint. Regenerate documentation with roxygen2 8.1.1 when documentation is next changed and review the generated diff. Build/test Python declarations permit the current maturin, pytest, polars, PyArrow, pandas and duckdb releases; test their supported Python range when updating them.

## Upgrades constrained by current parents

The following list covers every newer stable Rust line identified in the 2026-11-02 review after the compatible updates above. These are pending updates, not audit exclusions. Each listed requirement comes from the resolved parent package's published manifest. Recheck it on a parent release, and rerun `cargo metadata --locked --format-version 1` for both workspaces when assessing an upgrade. Multiple major versions already coexist for some packages; updating one does not remove the older required copy.

| Package | Locked → newer stable | Blocking parent requirement |
|---|---|---|
| `arrow-array` | 59.3.2 → 60.1.0 | `arrow-cast 59.3.2: ^59.3.2`; `arrow-ipc 59.3.2: ^59.3.2`; `arrow-ord 59.3.2: ^59.3.2`; `arrow-select 59.3.2: ^59.3.2`; `ckdata 0.0.1: ^59`; `ckdata-r 0.0.1: ^59`; `parquet 59.3.2: ^59.3.2`; `pyo3-arrow 0.20.0: ^59` |
| `arrow-buffer` | 59.3.2 → 60.1.0 | `arrow-array 59.3.2: ^59.3.2`; `arrow-cast 59.3.2: ^59.3.2`; `arrow-data 59.3.2: ^59.3.2`; `arrow-ipc 59.3.2: ^59.3.2`; `arrow-ord 59.3.2: ^59.3.2`; `arrow-select 59.3.2: ^59.3.2`; `parquet 59.3.2: ^59.3.2`; `pyo3-arrow 0.20.0: ^59` |
| `arrow-cast` | 59.3.2 → 60.1.0 | `pyo3-arrow 0.20.0: ^59` |
| `arrow-data` | 59.3.2 → 60.1.0 | `arrow-array 59.3.2: ^59.3.2`; `arrow-cast 59.3.2: ^59.3.2`; `arrow-ipc 59.3.2: ^59.3.2`; `arrow-ord 59.3.2: ^59.3.2`; `arrow-select 59.3.2: ^59.3.2`; `parquet 59.3.2: ^59.3.2`; `pyo3-arrow 0.20.0: ^59` |
| `arrow-ipc` | 59.3.2 → 60.1.0 | `parquet 59.3.2: ^59.3.2` |
| `arrow-ord` | 59.3.2 → 60.1.0 | `arrow-cast 59.3.2: ^59.3.2`; `ckdata 0.0.1: ^59` |
| `arrow-schema` | 59.3.2 → 60.1.0 | `arrow-array 59.3.2: ^59.3.2`; `arrow-cast 59.3.2: ^59.3.2`; `arrow-data 59.3.2: ^59.3.2`; `arrow-ipc 59.3.2: ^59.3.2`; `arrow-ord 59.3.2: ^59.3.2`; `arrow-select 59.3.2: ^59.3.2`; `ckdata 0.0.1: ^59`; `parquet 59.3.2: ^59.3.2`; `pyo3-arrow 0.20.0: ^59` |
| `arrow-select` | 59.3.2 → 60.1.0 | `arrow-cast 59.3.2: ^59.3.2`; `arrow-ipc 59.3.2: ^59.3.2`; `arrow-ord 59.3.2: ^59.3.2`; `ckdata 0.0.1: ^59`; `parquet 59.3.2: ^59.3.2`; `pyo3-arrow 0.20.0: ^59` |
| `atoi` | 2.0.0 → 3.1.1 | `arrow-cast 59.3.2: ^2.0.0` |
| `block-buffer` | 0.10.4 → 0.12.1 | `digest 0.10.7: ^0.10` |
| `comfy-table` | 7.2.2 → 8.0.1 | `arrow-cast 59.3.2: ^7` |
| `cpufeatures` | 0.2.17 → 0.3.1 | `sha1 0.10.7: ^0.2` |
| `crypto-common` | 0.1.7 → 0.2.2 | `digest 0.10.7: ^0.1.3` |
| `digest` | 0.10.7 → 0.11.3 | `crc-fast 1.10.0: ^0.10`; `sha1 0.10.7: ^0.10.7` |
| `generic-array` | 0.14.7 → 1.4.5 | `block-buffer 0.10.4: ^0.14`; `crypto-common 0.1.7: =0.14.7` |
| `getrandom` | 0.2.17 → 0.4.3 | `const-random-macro 0.1.16: ^0.2.0`; `ring 0.17.14: ^0.2.10` |
| `getrandom` | 0.3.4 → 0.4.3 | `ahash 0.8.12: ^0.3.1` |
| `http` | 0.2.12 → 1.5.0 | `aws-smithy-protocol-test 0.64.0: ^0.2.12` |
| `lru` | 0.18.5 → 0.19.0 | `aws-sdk-s3 1.157.0: ^0.18.2` |
| `nom` | 7.1.3 → 8.0.0 | `cbor-diag 0.1.12: ^7.1.3` |
| `num-bigint` | 0.4.8 → 0.5.1 | `cbor-diag 0.1.12: ^0.4.3`; `num-rational 0.4.2: ^0.4.0` |
| `object` | 0.39.1 → 0.40.0 | `ar_archive_writer 0.5.3: ^0.39` |
| `parquet` | 59.3.2 → 60.1.0 | `ckdata 0.0.1: ^59` |
| `phf` | 0.12.1 → 0.14.0 | `chrono-tz 0.10.4: ^0.12` |
| `phf_shared` | 0.12.1 → 0.14.0 | `phf 0.12.1: ^0.12.0` |
| `r-efi` | 5.3.0 → 7.1.0 | `getrandom 0.3.4: ^5.1` |
| `r-efi` | 6.0.0 → 7.1.0 | `getrandom 0.4.3: ^6` |
| `roxmltree` | 0.14.1 → 0.21.2 | `aws-smithy-protocol-test 0.64.0: ^0.14.1` |
| `sha1` | 0.10.7 → 0.11.0 | `aws-config 1.13.0: ^0.10` |
| `spin` | 0.10.1 → 0.12.3 | `crc-fast 1.10.0: ^0.10.0` |
| `syn` | 2.0.119 → 3.0.9 | `aws-smithy-runtime-api-macros 1.2.0: ^2.0.114`; `extendr-macros 0.9.0: ^2.0.109`; `recursive-proc-macro-impl 0.1.1: ^2.0`; `tracing-attributes 0.1.31: ^2.0`; `windows-implement 0.60.2: ^2.0`; `windows-interface 0.59.3: ^2.0`; `zerocopy-derive 0.8.63: ^2.0.46` |
| `wasi` | 0.11.1+wasi-snapshot-preview1 → 0.14.7+wasi-0.2.4 | `getrandom 0.2.17: ^0.11`; `mio 1.2.4: ^0.11.0` |
| `wasip2` | 1.0.4+wasi-0.2.12 → 2.0.1+wasi-0.2.12 | `getrandom 0.3.4: ^1` |
| `windows-core` | 0.62.2 → 0.100.0 | `iana-time-zone 0.1.65: >=0.56, <=0.62` |
| `windows-implement` | 0.60.2 → 0.100.0 | `windows-core 0.62.2: ^0.60.2` |
| `windows-interface` | 0.59.3 → 0.100.0 | `windows-core 0.62.2: ^0.59.3` |
| `windows-link` | 0.2.1 → 0.100.0 | `chrono 0.4.45: ^0.2`; `windows-core 0.62.2: ^0.2.1`; `windows-result 0.4.1: ^0.2.1`; `windows-strings 0.5.1: ^0.2.1`; `windows-sys 0.61.2: ^0.2.1` |
| `windows-result` | 0.4.1 → 0.100.0 | `windows-core 0.62.2: ^0.4.1` |
| `windows-strings` | 0.5.1 → 0.100.0 | `windows-core 0.62.2: ^0.5.1` |
| `windows-sys` | 0.52.0 → 0.61.2 | `ring 0.17.14: ^0.52` |
| `windows-targets` | 0.52.6 → 0.53.5 | `windows-sys 0.52.0: ^0.52.0` |
| `windows_aarch64_gnullvm` | 0.52.6 → 0.53.1 | `windows-targets 0.52.6: ^0.52.6` |
| `windows_aarch64_msvc` | 0.52.6 → 0.53.1 | `windows-targets 0.52.6: ^0.52.6` |
| `windows_i686_gnu` | 0.52.6 → 0.53.1 | `windows-targets 0.52.6: ^0.52.6` |
| `windows_i686_gnullvm` | 0.52.6 → 0.53.1 | `windows-targets 0.52.6: ^0.52.6` |
| `windows_i686_msvc` | 0.52.6 → 0.53.1 | `windows-targets 0.52.6: ^0.52.6` |
| `windows_x86_64_gnu` | 0.52.6 → 0.53.1 | `windows-targets 0.52.6: ^0.52.6` |
| `windows_x86_64_gnullvm` | 0.52.6 → 0.53.1 | `windows-targets 0.52.6: ^0.52.6` |
| `windows_x86_64_msvc` | 0.52.6 → 0.53.1 | `windows-targets 0.52.6: ^0.52.6` |
| `wit-bindgen` | 0.57.1 → 0.63.0 | `wasip2 1.0.4+wasi-0.2.12: ^0.57.1` |

Arrow and Parquet move together with pyo3-arrow. The latest `pyo3-arrow 0.20.0` still requires `arrow-* ^59`, so Arrow 60.1.0 requires a compatible binding release or a deliberate binding change. Keep the grouped Renovate update visible until that work is possible.

The Windows 0.100.0 family also needs an MSRV decision: `windows-core`, `windows-implement`, `windows-interface`, `windows-result`, and `windows-strings` require Rust 1.95, above the declared 1.94.1 minimum. `windows-link 0.100.0` requires 1.85, but its current parents still request 0.2. A compatible parent release alone does not authorize raising the project minimum. Test the declared minimum and the supported Windows R build before changing it.
