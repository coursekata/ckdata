# Dependency maintenance

The repository owner listed in [CODEOWNERS](CODEOWNERS) reviews dependency findings. Review the complete inventory on dependency pull requests, before a release, and during the monthly lock maintenance. Recheck the deferred items below by 2026-12-01, or sooner when a parent package releases, an advisory changes, or the affected feature becomes reachable from untrusted input.

The shared quality workflow gates fixed HIGH/CRITICAL findings. The repository inventory retains UNKNOWN, LOW and MEDIUM findings, unfixed advisories, and development dependencies. OSV also reports Rust maintenance advisories that Trivy's vulnerability database may omit. No severity or fix filter belongs in the inventory. A clean Trivy result alone is insufficient to close a maintenance finding.

CI uses OSV-Scanner directly on both Cargo locks, `uv.lock`, the repository
tools' `.github/package-lock.json`, and the installed Renovate validator.
Native JSON reports retain all packages, all vulnerabilities
and Rust maintenance advisories. The npm dependency-tree output is retained
alongside the scanner report, including installation-tree errors. No generated
requirements file or custom registry API client is used.

To audit the project locks locally with [OSV-Scanner](https://google.github.io/osv-scanner/installation/):

```sh
osv-scanner scan source --lockfile Cargo.lock --lockfile r/src/rust/Cargo.lock \
  --lockfile uv.lock --all-packages --all-vulns --format json \
  --output-file /tmp/ckdata-dependency-audit.json
```

OSV's exit status 1 means findings exist; other unsuccessful statuses fail CI.
Reports remain available for review. Trivy scans each real lockfile directly with
`--include-dev-deps`; the inventory includes every severity and unfixed finding,
and a separate invocation gates fixed HIGH/CRITICAL findings. No test constructs
a replacement dependency graph to verify the upstream scanners.

A finding is closed when the dependency no longer resolves to an affected version, with a fresh audit attached. For a finding that cannot yet be fixed, record the exact advisory, package, parent constraint, owner and next review trigger here. Keep it visible in reports. Do not add an ignore rule, widen an upstream dependency constraint, edit a generated hook environment, or use an unreviewed fork to make a report pass. New vulnerability findings need an explicit risk decision by the repository owner; the entries below do not accept other advisories or future versions automatically.

## Current findings requiring follow-up

Reviewed 2026-11-07 for the Just/Lefthook migration. All entries remain visible
in the complete inventory. Native `npm audit fix` removed compatible transitive
findings without changing Renovate's pinned version. The remaining six affected
package entries represent the three existing advisories below, including their
dependency paths; there are no HIGH or CRITICAL npm findings in this inventory.

| Finding | Scope and present constraint | Action and review trigger |
|---|---|---|
| [RUSTSEC-2024-0436](https://rustsec.org/advisories/RUSTSEC-2024-0436.html), informational/unmaintained | `paste 1.0.15`, required by the current `extendr-api 0.9.0`. No patched paste release. This advisory reports maintenance status and assigns no disclosed vulnerability or CVSS score. | Monitor extendr for a release that removes/replaces paste. Reevaluate the R binding dependency if a security advisory appears, the replacement is released, or maintenance stops blocking an upgrade. Review monthly; do not fork extendr solely to silence this informational finding. |
| [GHSA-3g3m-2fjr-6pw4](https://github.com/advisories/GHSA-3g3m-2fjr-6pw4), moderate | Renovate `44.206.2` pins affected `glob 13.0.6`; `glob 14.0.2` is the first fix. Existing transitive copies must be checked independently. | Update the npm development dependency when a tested Renovate release adopts the fix; audit the installed graph after every validator update. |
| [GHSA-g3hv-75wg-m249](https://github.com/advisories/GHSA-g3hv-75wg-m249), moderate | Renovate `44.206.2` pins affected `minimatch 10.2.6`; `minimatch 11.0.1` is the first fix. | Same review trigger. Retain every installed affected version and dependency path in audit evidence. |
| [GHSA-gcqx-w48m-w2h4](https://github.com/advisories/GHSA-gcqx-w48m-w2h4), low | Renovate `44.206.2` pins `deepmerge 4.3.1`, and no fixed release exists. | Monitor deepmerge and Renovate releases monthly and when the advisory changes. Reevaluate use of the validator if the affected merge operation is found to process attacker-controlled configuration. |

These npm findings concern the developer validator. Published [Renovate 44.206.2 metadata](https://registry.npmjs.org/renovate/44.206.2) still pins all three affected direct versions. Moving it from a hook environment into npm does not close them. No repository npm overrides or cache edits are maintained.

The validator also retains upstream deprecations: `bunyan -> mv -> rimraf
2.4.5 -> glob 6.0.4 -> inflight 1.0.6`, `global-agent -> boolean 3.2.0`, and
`google-auth-library -> gaxios -> node-fetch -> fetch-blob -> node-domexception
1.0.0`. These are development-tool dependencies, not published ckdata runtime
dependencies. Reevaluate them with the validator update; do not patch its
installed dependency tree or hide the package-manager warnings.

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

The shared Python development environment is declared in root `pyproject.toml`
and pinned in `uv.lock`. The default group covers pytest and the repository
tools (Just, Lefthook, Ruff, Air, identify and the upstream file checkers).
`bindings` adds the native build, PyArrow and frame-library tools. The Python
package metadata remains under `python/`. No test has a separate lockfile.
The workspace's `tool.uv.exclude-newer` enforces a seven-day wait during ordinary
updates and lock maintenance. Refresh with:

```sh
uv lock --upgrade
uv run --locked pytest
uv run --locked --group bindings --all-packages pytest python/tests
```

The Renovate validator is declared separately in `.github/package.json` and
locked with npm, including its supported Node 24 runtime. Run
`npm update --prefix .github`, `just _setup-repository-tools`,
`npm audit --prefix .github`, and `just lint --target repo` when maintaining it.
Only Node's runtime installer and RE2's native build scripts are enabled.
Compatible npm fixes use `npm audit fix`; never use `--force` to accept a
breaking change or the scanner's suggested downgrade of Renovate.

Renovate's PEP621 manager supports the shared uv workspace lock and monthly
lock maintenance. The former custom script-lock updater has been removed;
[Renovate's standard support](https://docs.renovatebot.com/modules/manager/pep621/)
owns Python dependency and lock refreshes. The retired AWS probe's boto3 and
botocore dependencies are no longer part of the development environment.

The dependency inventory covers every public registry version in both Cargo
locks and `uv.lock`, including optional development groups and platform branches.
Scanner coverage is verified; fixed HIGH/CRITICAL findings fail CI. Informational,
low, moderate and unfixed advisories remain visible for owner review.

R package dependencies and isolated Python build-backend resolution are not
represented as locked by `uv.lock`. Retain R `sessionInfo()` and actual package
build/test versions in release evidence. Review CRAN versions for bit64,
nanoarrow, tibble and testthat monthly; Renovate has no native CRAN manager.
`RoxygenNote` records the generator used rather than an install constraint.
Regenerate documentation with roxygen2 8.1.1 when documentation next changes.

## Upgrades constrained by current parents

`aws-config 1.13.0` exposes one private SSO token error through the deprecated
`Error::cause()` method instead of `Error::source()`; see its
[SSO token implementation](https://github.com/awslabs/aws-sdk-rust/blob/main/sdk/aws-config/src/sso/token.rs).
The core follows that legacy link to the cache error's typed I/O source so a
filesystem failure does not trigger browser login. Recheck this compatibility
fallback on AWS SDK updates and remove it when the SDK exposes the modern source
chain. Offline auth tests exercise the actual SDK wrapper with a filesystem fault.

The following list covers every newer stable Rust line identified in the 2026-11-04 review after the coordinated Arrow 60.1.0 and pyo3-arrow 0.21.0 upgrade. These are pending updates, not audit exclusions. Each listed requirement comes from the resolved parent package's published manifest. Recheck it on a parent release, and rerun `cargo metadata --locked --format-version 1` for both workspaces when assessing an upgrade. Multiple major versions already coexist for some packages; updating one does not remove the older required copy.

| Package | Locked → newer stable | Blocking parent requirement |
|---|---|---|
| `block-buffer` | 0.10.4 → 0.12.1 | `digest 0.10.7: ^0.10` |
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
| `phf` | 0.12.1 → 0.14.0 | `chrono-tz 0.10.4: ^0.12` |
| `phf_shared` | 0.12.1 → 0.14.0 | `phf 0.12.1: ^0.12.0` |
| `r-efi` | 5.3.0 → 7.1.0 | `getrandom 0.3.4: ^5.1` |
| `r-efi` | 6.0.0 → 7.1.0 | `getrandom 0.4.3: ^6` |
| `roxmltree` | 0.14.1 → 0.21.2 | `aws-smithy-protocol-test 0.64.0: ^0.14.1` |
| `sha1` | 0.10.7 → 0.11.0 | `aws-config 1.13.0: ^0.10` |
| `spin` | 0.10.1 → 0.12.3 | `crc-fast 1.10.0: ^0.10.0` |
| `syn` | 2.0.119 → 3.0.9 | `aws-smithy-runtime-api-macros 1.2.0: ^2.0.114`; `extendr-macros 0.9.0: ^2.0.109`; `tracing-attributes 0.1.31: ^2.0`; `windows-implement 0.60.2: ^2.0`; `windows-interface 0.59.3: ^2.0`; `zerocopy-derive 0.8.63: ^2.0.46` |
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

Arrow and Parquet move together with pyo3-arrow. `pyo3-arrow 0.21.0`, published on 2026-11-04, supports Arrow 60.1.0 and PyO3 0.30. The earlier Arrow 59 constraint is removed in both workspaces. The grouped Renovate update remains configured for future releases. This release becomes seven days old on 2026-11-11; local compatibility checks do not waive the publication waiting period.

The Windows 0.100.0 family also needs an MSRV decision: `windows-core`, `windows-implement`, `windows-interface`, `windows-result`, and `windows-strings` require Rust 1.95, above the declared 1.94.1 minimum. `windows-link 0.100.0` requires 1.85, but its current parents still request 0.2. A compatible parent release alone does not authorize raising the project minimum. Test the declared minimum and the supported Windows R build before changing it.
