"""Check source-package portability without building a native library."""

import io
import tarfile

import pytest

from scripts.check_r_tarball import check_tarball


@pytest.mark.parametrize("kind", ["valid", "missing_core", "symlink", "hardlink"])
def test_self_contained_r_tarball(tmp_path, kind):
    path = tmp_path / "ckdata_0.0.1.tar.gz"
    with tarfile.open(path, "w:gz") as archive:
        name = (
            "ckdata/DESCRIPTION"
            if kind == "missing_core"
            else "ckdata/src/rust/core/Cargo.toml"
        )
        entry = tarfile.TarInfo(name)
        entry.size = 1
        archive.addfile(entry, io.BytesIO(b"x"))
        if kind in {"symlink", "hardlink"}:
            entry = tarfile.TarInfo("ckdata/link")
            entry.type = tarfile.SYMTYPE if kind == "symlink" else tarfile.LNKTYPE
            entry.linkname = name
            archive.addfile(entry)
    if kind == "valid":
        check_tarball(path)
    else:
        with pytest.raises(ValueError):
            check_tarball(path)
