"""Check license policy, notice collection, and archive boundaries with fixtures."""

import io
import json
import tarfile
import zipfile

import pytest

from scripts import check_licenses

NOTICE_FILENAMES = (
    "LICENSE",
    "THIRD_PARTY_NOTICES.txt",
    "RUST_RUNTIME_NOTICES.html",
)


@pytest.mark.parametrize(
    "expression,selected",
    [
        ("Apache-2.0 / MIT", ["MIT"]),
        ("MIT AND Apache-2.0 OR ISC", ["Apache-2.0", "MIT"]),
        ("MIT AND (GPL-3.0 OR Apache-2.0)", ["Apache-2.0", "MIT"]),
        ("MIT AND GPL-3.0 OR ISC", ["ISC"]),
        (
            "GPL-3.0 OR Apache-2.0 WITH LLVM-exception",
            ["Apache-2.0 WITH LLVM-exception"],
        ),
    ],
)
def test_selected_licenses_preserve_supported_policy(expression, selected):
    assert check_licenses.selected_licenses(expression) == selected


@pytest.mark.parametrize(
    "expression",
    [
        "",
        "MIT AND",
        "(MIT",
        "MIT)",
        "OR MIT",
        "MIT WITH",
        "GPL-3.0",
        "MIT WITH unknown",
    ],
)
def test_selected_licenses_reject_incomplete_or_unreviewed_options(expression):
    with pytest.raises(ValueError):
        check_licenses.selected_licenses(expression)


@pytest.fixture
def registry_package(tmp_path):
    root = tmp_path / "registry/src/example-1.0"
    root.mkdir(parents=True)
    (root / "Cargo.toml").touch()
    return root, {
        "name": "example",
        "version": "1.0",
        "manifest_path": str(root / "Cargo.toml"),
        "source": "registry+https://example.invalid/index",
    }


def test_package_texts_retain_alternatives_notices_and_source_headers(registry_package):
    root, package = registry_package
    files = {
        "LICENSE-MIT": "MIT license text\n",
        "LICENSE-LGPL": "Retained alternative license text\n",
        "NOTICE": "Bundled component notice\n",
        "licenses/component.txt": "Component license text\n",
        "lib.rs": "// Copyright Example Authors.\n// SPDX-License-Identifier: MIT\nfn value() {}",
        "unrelated.txt": "Ordinary documentation\n",
    }
    for name, content in files.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
    # Generated trees are excluded before any file decoding takes place.
    for name in (".git", "target"):
        (root / name).mkdir()
        (root / name / "LICENSE").write_bytes(b"\xff")
    (root / "binary.rs").write_bytes(b"\xff")
    texts = check_licenses.package_texts(package, {})
    assert texts == {
        name: content
        for name, content in files.items()
        if name not in {"lib.rs", "unrelated.txt"}
    } | {
        "lib.rs (source preamble)": "// Copyright Example Authors.\n// SPDX-License-Identifier: MIT\n"
    }


def test_package_texts_require_full_license_and_utf8_notices(registry_package):
    root, package = registry_package
    (root / "lib.rs").write_text("// Copyright Example Authors.\nfn value() {}")
    with pytest.raises(ValueError, match="Missing license text"):
        check_licenses.package_texts(package, {})
    (root / "AUTHORS").write_text("Permission is hereby granted, free of charge")
    assert "AUTHORS" in check_licenses.package_texts(package, {})
    (root / "NOTICE").write_bytes(b"\xff")
    with pytest.raises(ValueError, match="Non-UTF-8 notice"):
        check_licenses.package_texts(package, {})


def test_package_texts_keep_registry_and_declared_file_boundaries(
    registry_package, tmp_path
):
    root, package = registry_package
    (root / "LICENSE").write_text("MIT license text\n")
    outside = tmp_path / "outside-notice"
    outside.write_text("Outside content\n")
    (root / "NOTICE").symlink_to(outside)
    assert "NOTICE" not in check_licenses.package_texts(package, {})
    package["license_file"] = str(outside)
    with pytest.raises(ValueError, match="License file escapes registry crate"):
        check_licenses.package_texts(package, {})
    declared = root / "legal" / "grant.txt"
    declared.parent.mkdir()
    declared.write_text("Declared license text\n")
    package["license_file"] = str(declared)
    assert check_licenses.package_texts(package, {})["legal/grant.txt"] == (
        "Declared license text\n"
    )
    package["source"] = "git+https://example.invalid/crate"
    with pytest.raises(ValueError, match="Unsupported dependency source"):
        check_licenses.package_texts(package, {})


@pytest.mark.parametrize("changed", [None, "revision", "snapshot"])
def test_upstream_supplements_require_exact_provenance(
    registry_package, tmp_path, monkeypatch, changed
):
    root, package = registry_package
    snapshot = tmp_path / "snapshot.txt"
    snapshot.write_text("Exact upstream license text\n")
    monkeypatch.setattr(check_licenses, "SUPPLEMENTS", tmp_path / "upstream.json")
    (root / ".cargo_vcs_info.json").write_text(
        json.dumps({"git": {"sha1": "reviewed"}})
    )
    record = {
        "revision": "reviewed",
        "files": [
            {
                "file": snapshot.name,
                "sha256": check_licenses.digest(snapshot.read_bytes()),
                "url": "https://example.invalid/LICENSE",
            }
        ],
    }
    if changed == "revision":
        record["revision"] = "other"
    elif changed == "snapshot":
        snapshot.write_text("Changed upstream license text\n")
    supplements = {"example@1.0": record}
    if changed:
        with pytest.raises(ValueError, match="Upstream .* changed"):
            check_licenses.package_texts(package, supplements)
    else:
        assert check_licenses.package_texts(package, supplements) == {
            "upstream: https://example.invalid/LICENSE": snapshot.read_text()
        }


@pytest.fixture
def expected_notices():
    return {
        "python/LICENSE": b"MIT license\n",
        "python/THIRD_PARTY_NOTICES.txt": b"Python dependency notices\n",
        "python/RUST_RUNTIME_NOTICES.html": b"<p>Python runtime notices</p>\n",
        "r/inst/LICENSE-MIT": b"MIT license\n",
        "r/inst/THIRD_PARTY_NOTICES.txt": b"R dependency notices\n",
        "r/inst/RUST_RUNTIME_NOTICES.html": b"<p>R runtime notices</p>\n",
    }


@pytest.mark.parametrize(
    "changed,filename",
    [(None, None), ("expression", None), ("metadata", None)]
    + [
        (changed, filename)
        for changed in ("declaration", "notice")
        for filename in NOTICE_FILENAMES
    ],
)
def test_wheel_license_metadata_and_contents(
    tmp_path, expected_notices, changed, filename
):
    path = tmp_path / "example.whl"
    metadata = "License-Expression: MIT\n" + "".join(
        f"License-File: {name.removeprefix('python/')}\n"
        for name in expected_notices
        if name.startswith("python/")
    )
    if changed == "expression":
        metadata = metadata.replace("License-Expression: MIT\n", "")
    elif changed == "declaration":
        metadata = metadata.replace(f"License-File: {filename}\n", "")
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr("example.dist-info/METADATA", metadata)
        if changed == "metadata":
            archive.writestr("other.dist-info/METADATA", metadata)
        for name, content in expected_notices.items():
            if name.startswith("python/"):
                archive.writestr(
                    "example.dist-info/licenses/" + name.removeprefix("python/"),
                    b"stale"
                    if changed == "notice" and name == "python/" + filename
                    else content,
                )
    if changed:
        with pytest.raises(ValueError):
            check_licenses.check_archive(path, expected_notices)
    else:
        check_licenses.check_archive(path, expected_notices)


@pytest.mark.parametrize(
    "changed,filename,is_r",
    [
        (changed, filename, is_r)
        for changed, filename in [
            (None, None),
            ("root", None),
            ("symlink", "LICENSE"),
            ("hardlink", "LICENSE"),
        ]
        + [("notice", filename) for filename in NOTICE_FILENAMES]
        for is_r in (False, True)
    ]
    + [("expression", None, False)],
)
def test_source_archive_validates_notice_contents_types_and_root(
    tmp_path, expected_notices, is_r, changed, filename
):
    path = tmp_path / "example.tar.gz"
    files = {
        "example/" + (name.removeprefix("r/") if is_r else name): content
        for name, content in expected_notices.items()
        if name.startswith("r/" if is_r else "python/")
    }
    files["example/DESCRIPTION" if is_r else "example/PKG-INFO"] = (
        b"License-Expression: MIT\n"
    )
    if changed == "expression":
        files["example/PKG-INFO"] = (
            b"Metadata-Version: 2.4\nName: example\nVersion: 1.0\n"
        )
    if changed == "root":
        files["other/NOTICE"] = b"Other root\n"
    notice_name = (
        "example/"
        + (
            "inst/" + ("LICENSE-MIT" if filename == "LICENSE" else filename)
            if is_r
            else "python/" + filename
        )
        if filename is not None
        else None
    )
    with tarfile.open(path, "w:gz") as archive:
        if changed in {"symlink", "hardlink"}:
            target = notice_name.rsplit("/", 1)[0] + "/synthetic-notice-target"
            entry = tarfile.TarInfo(target)
            entry.size = len(files[notice_name])
            archive.addfile(entry, io.BytesIO(files[notice_name]))
        for name, content in files.items():
            entry = tarfile.TarInfo(name)
            if name == notice_name and changed in {"symlink", "hardlink"}:
                entry.type = (
                    tarfile.SYMTYPE if changed == "symlink" else tarfile.LNKTYPE
                )
                entry.linkname = (
                    target.rsplit("/", 1)[-1] if changed == "symlink" else target
                )
                archive.addfile(entry)
            else:
                content = (
                    b"stale" if name == notice_name and changed == "notice" else content
                )
                entry.size = len(content)
                archive.addfile(entry, io.BytesIO(content))
    if changed in {"symlink", "hardlink"}:
        with tarfile.open(path, "r:gz") as archive:
            assert archive.extractfile(notice_name).read() == files[notice_name]
    if changed:
        message = (
            "sdist lacks MIT license expression" if changed == "expression" else None
        )
        with pytest.raises(ValueError, match=message):
            check_licenses.check_archive(path, expected_notices)
    else:
        check_licenses.check_archive(path, expected_notices)


def test_copyright_header_does_not_copy_api_example_keys():
    header = "// Copyright Example Authors.\n// SPDX-License-Identifier: MIT\n"
    assert (
        check_licenses.preamble(
            header + "\n//! API example: key = invented-public-sample\nfn main() {}"
        )
        == header
    )
    assert (
        check_licenses.preamble(
            "/* Copyright Example Authors.\n * License terms remain here.\n */\nfn main() {}"
        )
        == "/* Copyright Example Authors.\n * License terms remain here.\n */\n"
    )
