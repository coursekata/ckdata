# Releasing

## First-release prerequisites

The current release workflow publishes the R package. Python wheels and PyPI
publication need their own implementation and acceptance before Python can be
released.

R publication is disabled by `R_RELEASE_READY` in
[`release.yml`](../.github/workflows/release.yml). Enable it in the reviewed
first-release change after the public R query interface and package checks pass.
Add the package to the CourseKata R-universe registry with `subdir: r` and
`branch: "*release"`. The tagged commit must have successful main-branch CI.

## Publish an R release

1. Set the new version in `core/Cargo.toml`, `python/Cargo.toml`,
   `r/src/rust/Cargo.toml` and `r/DESCRIPTION` in one pull request, and merge it.
2. Tag the merge commit with a lightweight `vX.Y.Z` tag and push the tag.
3. The release workflow verifies that the tag matches all four versions and
   belongs to `main`, then creates the GitHub release.
4. R-universe builds the R binaries from that release. The workflow waits for
   the binaries, installs each without Rust present, and runs the package tests.

The verification matrix uses the current R release on Ubuntu 26.04 x86_64,
macOS arm64 and Windows x86_64. The Linux runner must match R-universe's binary
distribution. Run [`verify_release`](../.github/workflows/verify_release.yml)
manually to repeat verification for a version.

Never move a release tag. Publish a new version for a fix. Update the
[installation guide](installation.md) after the first release passes binary
verification.

## License notices

Both packages ship license notices. After changing a Cargo lock, run
`python3 scripts/check_licenses.py --write` and review the diff; CI rejects stale
notices. When changing the release compiler, run
`python3 scripts/check_licenses.py --runtime-write --write` and review compiler
provenance and licenses. `--archive /absolute/path/to/package` checks notices in
a wheel, Python source archive or R tarball. See
[dependency maintenance](../.github/dependency_policy.md) for ownership and audit
policy.
