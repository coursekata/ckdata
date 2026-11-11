"""Select owned files and invoke upstream checkers without changing check inputs."""

import argparse
import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from identify.identify import tags_from_path

EXCLUDE_PRIVATE = ":(glob,exclude)**/*[sS][aA][gG][aA]*/**"
LEGAL = {"THIRD_PARTY_LICENSES.txt", "NOTICE"}


def files(*, staged=False):
    command = (
        ["diff", "--cached", "--name-only", "--diff-filter=ACMR", "-z"]
        if staged
        else ["ls-files", "--cached", "--others", "--exclude-standard", "-z"]
    )
    result = subprocess.run(
        ["git", *command, "--", ".", EXCLUDE_PRIVATE],
        check=True,
        capture_output=True,
    )
    paths = sorted({Path(p.decode()) for p in result.stdout.split(b"\0") if p})
    if staged:
        return paths
    return [p for p in paths if regular_file(p)]


def regular_file(path):
    # Check parents from the repository outward before inspecting children.
    # A tracked directory may have been replaced with a symlink locally.
    return (
        not any(p.is_symlink() for p in [*reversed(path.parents), path])
        and path.is_file()
    )


def run(module, paths, *arguments):
    if not paths:
        return 0
    return subprocess.run(
        [
            sys.executable,
            "-m",
            "pre_commit_hooks." + module,
            *arguments,
            *map(str, paths),
        ],
        check=False,
    ).returncode


def style(paths, *, fix):
    paths = [
        p for p in paths if p.name not in LEGAL and "text" in tags_from_path(str(p))
    ]
    if fix:
        # These hooks return 1 when they successfully change a file.
        statuses = [
            run(tool, paths)
            for tool in ("trailing_whitespace_fixer", "end_of_file_fixer")
        ]
        return max(0 if status == 1 else status for status in statuses)
    with tempfile.TemporaryDirectory(prefix="ckdata-style-") as temporary:
        copies = []
        for path in paths:
            target = Path(temporary) / path
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, target)
            copies.append(target)
        return max(
            run(tool, copies)
            for tool in ("trailing_whitespace_fixer", "end_of_file_fixer")
        )


def lint(paths):
    text = [p for p in paths if "text" in tags_from_path(str(p))]
    return max(
        run("check_merge_conflict", text),
        run("check_yaml", [p for p in paths if p.suffix in {".yml", ".yaml"}]),
        run("check_toml", [p for p in paths if p.suffix == ".toml"]),
        run(
            "check_added_large_files",
            [p for p in paths if p.name not in LEGAL],
            "--enforce-all",
        ),
    )


def secrets(paths):
    # Scan the exact staged blobs. A private-path exclusion must happen in Git,
    # before a scanner reads patches, rather than in a scanner's allowlist.
    with tempfile.TemporaryDirectory(prefix="ckdata-staged-") as temporary:
        for path in paths:
            blob = subprocess.run(
                ["git", "show", ":" + path.as_posix()], check=True, capture_output=True
            )
            target = Path(temporary) / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(blob.stdout)
        if not paths:
            return 0
        return subprocess.run(
            ["gitleaks", "dir", "--no-banner", "--redact", temporary], check=False
        ).returncode


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("style", "lint", "secrets", "r-files"))
    parser.add_argument("--fix", action="store_true")
    args = parser.parse_args()
    if args.fix and args.command != "style":
        parser.error("--fix is only supported for style")
    paths = files(staged=args.command == "secrets")
    if args.command == "r-files":
        print(
            json.dumps(
                [
                    p.as_posix()
                    for p in paths
                    if p.suffix.lower() == ".r"
                    and p.as_posix() != "r/R/extendr-wrappers.R"
                ]
            )
        )
        return 0
    if args.command == "style":
        return style(paths, fix=args.fix)
    return {"lint": lint, "secrets": secrets}[args.command](paths)


if __name__ == "__main__":
    raise SystemExit(main())
