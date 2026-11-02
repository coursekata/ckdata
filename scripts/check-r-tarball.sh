#!/usr/bin/env sh
# Fails unless an R source tarball carries the core as real files. R-universe builds every binary
# from the tarball alone, so a core left behind as a symlink, or not copied at all, breaks them.
# Usage: scripts/check-r-tarball.sh <package>_<version>.tar.gz
set -eu
tarball=$1
pkg=$(basename "$tarball" | sed 's/_.*//')
if ! tar -tzf "$tarball" | grep -qx "$pkg/src/rust/core/Cargo.toml"; then
  echo "::error::$tarball does not carry the core at $pkg/src/rust/core"
  exit 1
fi
if tar -tvzf "$tarball" | grep '^l'; then
  echo "::error::$tarball holds symlinks, which a tarball build cannot follow"
  exit 1
fi
echo "$tarball carries the core and no symlinks"
