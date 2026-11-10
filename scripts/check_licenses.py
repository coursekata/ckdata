"""Refresh/check distribution notices from the two locked Cargo graphs, offline.

Run with --write after changing a lockfile. Without --write, stale or missing
notices fail the check. --archive may be repeated for Python wheels/sdists and
R source tarballs. --runtime-write captures the selected release compiler's
standard-library notices; --runtime-check checks them against that compiler.
No package source outside the manifests' registry crates is
searched. Upstream omissions are supplied by version-pinned local snapshots.
"""

import argparse
import hashlib
import html
import json
import os
import re
import subprocess
import sys
import tarfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SUPPLEMENTS = ROOT / "scripts/licenses/upstream.json"
PREFERENCE = [
    "MIT",
    "Apache-2.0",
    "ISC",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "0BSD",
    "MIT-0",
    "BSL-1.0",
    "Zlib",
    "Unicode-3.0",
    "Unlicense",
    "CC0-1.0",
    "Apache-2.0 WITH LLVM-exception",
]
PREFIXES = ("license", "licence", "copying", "copyright", "notice", "authors")
SOURCE_SUFFIXES = {
    ".rs",
    ".c",
    ".h",
    ".cc",
    ".cpp",
    ".hpp",
    ".s",
    ".asm",
    ".inc",
    ".go",
}
HEADER = """Third-party notices for ckdata

CourseKata-owned code is licensed under MIT; see LICENSE. Dependencies retain
 their own licenses. The inventory below records upstream declarations and the
permissive license options selected for this distribution. AND conditions all
apply; OR conditions permit a choice. Unselected license texts are retained as
upstream documentation, including r-efi's LGPL alternative; r-efi is used under
MIT. Bundled components keep the additional conditions in their license texts.

This inventory conservatively covers every registry package in the locked Cargo
graph, including build, development and other-target dependencies. A package's
presence here does not mean it is linked into every binary. Rust standard-library and compiler runtime
notices are in RUST_RUNTIME_NOTICES.html. Separately installed Python/R
dependencies are outside this Cargo inventory.

License, copyright, AUTHORS and NOTICE files are reproduced below. Source-file
copyright/license preambles are also preserved. Identical texts share a SHA-256
reference. Upstream snapshots supply files omitted from published crate archives.
Refresh or verify with: python3 scripts/check_licenses.py [--write]
""".replace("\n their", "\ntheir")


def digest(data):
    return hashlib.sha256(data).hexdigest()


class _LicenseExpression:
    """Read the existing SPDX subset with AND precedence over OR."""

    def __init__(self, expression):
        self.expression = expression
        self.tokens = re.findall(r"\(|\)|[^\s()]+", expression.replace("/", " OR "))
        self.position = 0

    def take(self, token):
        if self.position < len(self.tokens) and self.tokens[self.position] == token:
            self.position += 1
            return True
        return False

    def atom(self):
        if self.position >= len(self.tokens):
            raise ValueError(f"Incomplete license expression: {self.expression}")
        token = self.tokens[self.position]
        self.position += 1
        if token == "(":
            value = self.either()
            if not self.take(")"):
                raise ValueError(f"Unbalanced license expression: {self.expression}")
            return value
        if token in {"AND", "OR", ")", "WITH"}:
            raise ValueError(f"Invalid license expression: {self.expression}")
        if self.take("WITH"):
            if self.position >= len(self.tokens):
                raise ValueError(f"Missing license exception: {self.expression}")
            token += " WITH " + self.tokens[self.position]
            self.position += 1
        return {token} if token in PREFERENCE else None

    def both(self):
        value = self.atom()
        while self.take("AND"):
            other = self.atom()
            value = value | other if value is not None and other is not None else None
        return value

    def either(self):
        choices = [self.both()]
        while self.take("OR"):
            choices.append(self.both())
        choices = [value for value in choices if value is not None]
        return (
            min(
                choices,
                key=lambda value: (
                    sum(PREFERENCE.index(x) for x in value),
                    sorted(value),
                ),
            )
            if choices
            else None
        )


def selected_licenses(expression):
    """Choose an allowed branch of an SPDX expression; preserve every AND term."""
    parser = _LicenseExpression(expression)
    value = parser.either()
    if parser.position != len(parser.tokens) or value is None:
        raise ValueError(f"No reviewed permissive option: {expression}")
    return sorted(value)


def metadata(manifest):
    command = [
        "cargo",
        "metadata",
        "--format-version",
        "1",
        "--locked",
        "--offline",
        "--manifest-path",
        str(manifest),
    ]
    return json.loads(subprocess.check_output(command, cwd=ROOT, text=True))


def preamble(text):
    lines = []
    block = False
    for line in text.splitlines(keepends=True):
        stripped = line.lstrip()
        if block:
            lines.append(line)
            if "*/" in line:
                block = False
        elif stripped.startswith(("//!", "///")) and any(
            re.search(r"copyright|SPDX-License-Identifier", previous, re.IGNORECASE)
            and not previous.lstrip().startswith(("//!", "///"))
            for previous in lines
        ):
            # The ordinary copyright header is complete. API documentation and
            # example keys below it are not part of the required legal notice.
            break
        elif not stripped.strip() or stripped.startswith("//"):
            lines.append(line)
        elif stripped.startswith("/*"):
            lines.append(line)
            block = "*/" not in stripped
        else:
            break
    result = "".join(lines).strip()
    return (
        result + "\n"
        if re.search(r"copyright|SPDX-License-Identifier", result, re.IGNORECASE)
        else None
    )


def _read_package_text(path, root, package_name, in_license_directory):
    if not path.is_file() or not path.resolve().is_relative_to(root):
        return None
    is_notice = path.name.lower().startswith(PREFIXES) or in_license_directory
    if not is_notice and path.suffix.lower() not in SOURCE_SUFFIXES:
        return None
    relative = path.relative_to(root).as_posix()
    try:
        content = path.read_bytes().decode("utf-8")
    except UnicodeDecodeError:
        if is_notice:
            raise ValueError(f"Non-UTF-8 notice: {package_name}/{relative}")
        return None
    if is_notice:
        return relative, content
    header = preamble(content)
    return (relative + " (source preamble)", header) if header else None


def _registry_texts(root, package_name):
    """Collect notices and source headers without following paths out of the crate."""
    texts = {}
    for parent, directories, files in os.walk(root, followlinks=False):
        directories[:] = sorted(
            name
            for name in directories
            if name not in {".git", "target"} and "saga" not in name.lower()
        )
        parent = Path(parent)
        in_license_directory = any(
            part.lower() in {"licenses", "licences"}
            for part in parent.relative_to(root).parts
        )
        for name in sorted(files):
            notice = _read_package_text(
                parent / name, root, package_name, in_license_directory
            )
            if notice:
                relative, content = notice
                texts[relative] = content
    return texts


def _supplement_texts(package, root, supplements):
    """Verify upstream revision and snapshot hashes before including omitted files."""
    key = package["name"] + "@" + package["version"]
    supplement = supplements.get(key)
    if not supplement:
        return {}
    vcs = json.loads((root / ".cargo_vcs_info.json").read_text())
    if vcs["git"]["sha1"] != supplement["revision"]:
        raise ValueError(f"Upstream revision changed: {key}")
    texts = {}
    for record in supplement["files"]:
        data = (SUPPLEMENTS.parent / record["file"]).read_bytes()
        if digest(data) != record["sha256"]:
            raise ValueError(f"Upstream snapshot changed: {record['file']}")
        texts["upstream: " + record["url"]] = data.decode("utf-8")
    return texts


def _is_license_text(name, content):
    # A copyright preamble alone cannot substitute for a complete license.
    return (
        bool(content.strip())
        and " (source preamble)" not in name
        and (
            name.startswith("upstream: ")
            or Path(name).name.lower().startswith(("license", "licence", "copying"))
            or any(
                part.lower() in {"licenses", "licences"} for part in Path(name).parts
            )
            # r-efi intentionally carries its full MIT grant in AUTHORS.
            or "Permission is hereby granted, free of charge" in content
        )
    )


def package_texts(package, supplements):
    root = Path(package["manifest_path"]).parent.resolve()
    # Cargo's registry packages are the only trees this script may inspect.
    if (
        not package["source"].startswith("registry+")
        or "registry" not in root.parts
        or "src" not in root.parts
    ):
        raise ValueError(f"Unsupported dependency source: {package['name']}")
    texts = _registry_texts(root, package["name"])
    license_file = package.get("license_file")
    if license_file:
        path = Path(license_file).resolve()
        if not path.is_relative_to(root):
            raise ValueError(f"License file escapes registry crate: {package['name']}")
        texts[path.relative_to(root).as_posix()] = path.read_text(encoding="utf-8")
    texts.update(_supplement_texts(package, root, supplements))
    if not any(_is_license_text(name, content) for name, content in texts.items()):
        key = package["name"] + "@" + package["version"]
        raise ValueError(
            f"Missing license text for {key}; add an exact-revision upstream snapshot"
        )
    return texts


def render(manifest, supplements):
    graph = metadata(manifest)
    records = []
    unique = {}
    owned_manifests = {
        (ROOT / name).resolve()
        for name in ("core/Cargo.toml", "python/Cargo.toml", "r/src/rust/Cargo.toml")
    }
    for package in sorted(
        graph["packages"], key=lambda item: (item["name"], item["version"])
    ):
        if package["source"] is None:
            if Path(package["manifest_path"]).resolve() not in owned_manifests:
                raise ValueError(f"Unreviewed local dependency: {package['name']}")
            if package.get("license") != "MIT":
                raise ValueError(f"Owned crate lacks MIT metadata: {package['name']}")
            continue
        expression = package.get("license")
        if not expression:
            raise ValueError(f"Missing license declaration: {package['name']}")
        selected = selected_licenses(expression)
        files = []
        for name, content in sorted(package_texts(package, supplements).items()):
            content_hash = digest(content.encode("utf-8"))
            unique[content_hash] = content
            files.append((name, content_hash))
        records.append((package, expression, selected, files))
    lock = manifest.parent / "Cargo.lock"
    output = [
        HEADER,
        f"\nCargo.lock SHA-256: {digest(lock.read_bytes())}\n",
        f"Registry packages: {len(records)}\n\nInventory\n=========\n",
    ]
    for package, expression, selected, files in records:
        output.extend(
            [
                f"\n{package['name']} {package['version']}\n",
                f"Source: {package['source']}\n",
                f"Repository: {package.get('repository') or '(not declared)'}\n",
                f"Declared license: {expression}\n",
                f"Selected license obligations: {' AND '.join(selected)}\n",
            ]
        )
        for name, content_hash in files:
            output.append(f"  {name}\n    Text SHA-256: {content_hash}\n")
    output.append("\nFull texts\n==========\n")
    for content_hash, content in sorted(unique.items()):
        output.extend([f"\n--- Text SHA-256: {content_hash} ---\n", content, "\n"])
    return "".join(output).encode("utf-8")


def runtime_notices():
    version = subprocess.check_output(
        ["rustc", "--version", "--verbose"], text=True
    ).strip()
    sysroot = Path(
        subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip()
    )
    docs = sysroot / "share/doc/rust"
    source = docs / "COPYRIGHT-library.html"
    if not source.is_file():
        raise ValueError(
            "Selected Rust toolchain lacks bundled standard-library copyright notices"
        )
    original = source.read_bytes()
    inputs = {"COPYRIGHT-library.html": digest(original)}
    # These cover the standard library's in-tree licenses and compiler runtime
    # exceptions. Dependency-specific notices are in COPYRIGHT-library.html.
    extra = []
    for name in (
        "Apache-2.0",
        "MIT",
        "Unicode-3.0",
        "BSD-2-Clause",
        "ISC",
        "NCSA",
        "LLVM-exception",
    ):
        path = docs / "licenses" / (name + ".txt")
        content = path.read_bytes()
        inputs["licenses/" + path.name] = digest(content)
        extra.append(
            "<h3>"
            + html.escape(name)
            + "</h3><pre>"
            + html.escape(content.decode("utf-8"))
            + "</pre>"
        )
    provenance = {"rustc": version, "inputs": inputs}
    introduction = (
        "<h2>ckdata release toolchain</h2><pre>" + html.escape(version) + "</pre>"
        "<p>These upstream notices conservatively cover the selected Rust toolchain's "
        "standard-library build inputs across targets. Presence in this document does "
        "not imply that a component is linked into every ckdata binary. Original "
        "licenses remain in force. The standard library's dual license is used under MIT.</p>"
        "<pre>Input SHA-256 values: "
        + html.escape(json.dumps(inputs, sort_keys=True, indent=2))
        + "</pre>"
    )
    content = original.decode("utf-8").replace("<body>", "<body>" + introduction, 1)
    content = content.replace(
        "</body>",
        "<h2>Standard-library and compiler runtime license texts</h2>"
        + "".join(extra)
        + "</body>",
    )
    data = content.encode("utf-8")
    provenance["notice_sha256"] = digest(data)
    return provenance, data


def expected_files():
    supplements = json.loads(SUPPLEMENTS.read_text())
    owned = (ROOT / "LICENSE").read_bytes()
    record = json.loads((SUPPLEMENTS.parent / "rust_runtime.json").read_text())
    runtime = (ROOT / "python/RUST_RUNTIME_NOTICES.html").read_bytes()
    if digest(runtime) != record["notice_sha256"]:
        raise ValueError(
            "Rust runtime notices changed; refresh with --runtime-write for the release compiler"
        )
    return {
        "python/RUST_RUNTIME_NOTICES.html": runtime,
        "r/inst/RUST_RUNTIME_NOTICES.html": runtime,
        "python/LICENSE": owned,
        "python/THIRD_PARTY_NOTICES.txt": render(ROOT / "Cargo.toml", supplements),
        "r/inst/LICENSE-MIT": owned,
        "r/inst/THIRD_PARTY_NOTICES.txt": render(
            ROOT / "r/src/rust/Cargo.toml", supplements
        ),
    }


def _check_wheel_licenses(path, expected):
    with zipfile.ZipFile(path) as archive:
        members = archive.namelist()
        matches = [name for name in members if name.endswith(".dist-info/METADATA")]
        if len(matches) != 1:
            raise ValueError(f"Unexpected wheel metadata: {path}")
        metadata_text = archive.read(matches[0]).decode("utf-8")
        if "License-Expression: MIT\n" not in metadata_text:
            raise ValueError("Wheel lacks MIT license expression")
        prefix = matches[0].removesuffix("METADATA") + "licenses/"
        for name in (
            "LICENSE",
            "THIRD_PARTY_NOTICES.txt",
            "RUST_RUNTIME_NOTICES.html",
        ):
            if (
                f"License-File: {name}\n" not in metadata_text
                or archive.read(prefix + name) != expected["python/" + name]
            ):
                raise ValueError(f"Missing or stale wheel license: {name}")


def _check_tarball_licenses(path, expected):
    with tarfile.open(path, "r:gz") as archive:
        members = {member.name: member for member in archive.getmembers()}
        roots = {name.split("/", 1)[0] for name in members}
        if len(roots) != 1:
            raise ValueError(f"Unexpected archive roots: {path}")
        prefix = next(iter(roots)) + "/"
        is_r = prefix + "DESCRIPTION" in members
        required = (
            {
                "inst/LICENSE-MIT": "r/inst/LICENSE-MIT",
                "inst/THIRD_PARTY_NOTICES.txt": "r/inst/THIRD_PARTY_NOTICES.txt",
                "inst/RUST_RUNTIME_NOTICES.html": "r/inst/RUST_RUNTIME_NOTICES.html",
            }
            if is_r
            else {
                "python/LICENSE": "python/LICENSE",
                "python/THIRD_PARTY_NOTICES.txt": "python/THIRD_PARTY_NOTICES.txt",
                "python/RUST_RUNTIME_NOTICES.html": "python/RUST_RUNTIME_NOTICES.html",
            }
        )
        for member_name, expected_name in required.items():
            member = members.get(prefix + member_name)
            if (
                member is None
                or not member.isfile()
                or archive.extractfile(member).read() != expected[expected_name]
            ):
                raise ValueError(f"Missing or stale archive license: {member_name}")
        if not is_r:
            package_info = (
                archive.extractfile(prefix + "PKG-INFO").read().decode("utf-8")
            )
            if "License-Expression: MIT\n" not in package_info:
                raise ValueError("sdist lacks MIT license expression")


def check_archive(path, expected):
    if path.suffix == ".whl":
        _check_wheel_licenses(path, expected)
    else:
        _check_tarball_licenses(path, expected)
    print(f"License archive check passed: {path}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--write", action="store_true", help="refresh notices and owned-license copies"
    )
    parser.add_argument(
        "--archive",
        type=Path,
        action="append",
        default=[],
        help="check a built wheel, sdist or R source tarball",
    )
    parser.add_argument(
        "--runtime-write",
        action="store_true",
        help="capture actual release compiler notices from its bundled documentation",
    )
    parser.add_argument(
        "--runtime-check",
        action="store_true",
        help="require runtime notices to match the selected release compiler",
    )
    args = parser.parse_args()
    if args.runtime_write or args.runtime_check:
        record, runtime = runtime_notices()
        record_path = SUPPLEMENTS.parent / "rust_runtime.json"
        if args.runtime_write:
            record_path.write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")
            for name in (
                "python/RUST_RUNTIME_NOTICES.html",
                "r/inst/RUST_RUNTIME_NOTICES.html",
            ):
                path = ROOT / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(runtime)
        elif json.loads(record_path.read_text()) != record:
            raise ValueError(
                "Rust runtime notices do not match selected compiler; use --runtime-write for a release build"
            )
    expected = expected_files()
    for name, content in expected.items():
        path = ROOT / name
        if args.write:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
        elif not path.is_file() or path.read_bytes() != content:
            raise ValueError(
                f"Stale {name}; run python3 scripts/check_licenses.py --write"
            )
    for path in args.archive:
        check_archive(path, expected)
    print("Locked Cargo notices and package-owned MIT license copies are current")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
