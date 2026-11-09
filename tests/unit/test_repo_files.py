"""Exercise file-checking glue with synthetic repositories and native checkers."""

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parents[2] / "scripts/repo-files.py"


@pytest.fixture
def repository(tmp_path):
    subprocess.run(["git", "init", "-q", str(tmp_path)], check=True)
    return tmp_path


def invoke(repository, *arguments, env=None):
    return subprocess.run(
        [sys.executable, str(SCRIPT), *arguments],
        cwd=repository,
        env={**os.environ, **(env or {})},
        capture_output=True,
        text=True,
        check=False,
    )


def test_style_check_reports_native_formatter_changes_without_editing_source(
    repository,
):
    source = repository / "example.py"
    source.write_bytes(b"answer = 42  \n\n")
    result = invoke(repository, "style")
    assert result.returncode == 1, result.stderr
    assert source.read_bytes() == b"answer = 42  \n\n"
    result = invoke(repository, "style", "--fix")
    assert result.returncode == 0, result.stderr
    assert source.read_bytes() == b"answer = 42\n"
    assert invoke(repository, "style").returncode == 0


def test_style_preserves_generated_notices_and_ignored_or_linked_files(repository):
    (repository / ".gitignore").write_text("ignored/\n")
    for path in (
        "ignored/example.py",
        "python/THIRD_PARTY_NOTICES.txt",
        "scripts/licenses/upstream/example.txt",
    ):
        target = repository / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(b"preserve this  \n\n")
    external = repository.parent / "external.txt"
    external.write_bytes(b"preserve this  \n\n")
    (repository / "linked.txt").symlink_to(external)
    tracked = repository / "tracked-directory"
    tracked.mkdir()
    (tracked / "external.txt").write_text("initial content\n")
    subprocess.run(
        ["git", "add", "tracked-directory/external.txt"], cwd=repository, check=True
    )
    (tracked / "external.txt").unlink()
    tracked.rmdir()
    tracked.symlink_to(external.parent, target_is_directory=True)
    result = invoke(repository, "style", "--fix")
    assert result.returncode == 0, result.stderr
    assert external.read_bytes() == b"preserve this  \n\n"
    assert (
        repository / "python/THIRD_PARTY_NOTICES.txt"
    ).read_bytes() == b"preserve this  \n\n"
    assert (repository / "ignored/example.py").read_bytes() == b"preserve this  \n\n"


@pytest.mark.parametrize(
    "filename,contents",
    [
        ("invalid.yaml", "example: ["),
        ("invalid.toml", "example = ["),
        ("large.bin", b"\0" * 513 * 1024),
    ],
    ids=["yaml", "toml", "large-file"],
)
def test_native_file_checks_reject_invalid_inputs(repository, filename, contents):
    target = repository / filename
    if isinstance(contents, bytes):
        target.write_bytes(contents)
    else:
        target.write_text(contents)
    result = invoke(repository, "lint")
    assert result.returncode != 0, result.stdout + result.stderr


def test_file_selection_prunes_private_directories_and_generated_r_bindings(repository):
    # These are entirely synthetic fixtures, containing no author-owned material.
    for path in ("r/R/example.R", "r/R/extendr-wrappers.R", "synthetic-saga/hidden.R"):
        target = repository / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text("answer <- 42\n")
    result = invoke(repository, "r-files")
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout) == ["r/R/example.R"]


def test_secret_scan_uses_staged_blobs_without_touching_unstaged_edits(repository):
    source = repository / "example.py"
    source.write_text("staged content\n")
    subprocess.run(["git", "add", "example.py"], cwd=repository, check=True)
    source.write_text("unstaged content\n")
    commands = repository / "bin"
    commands.mkdir()
    scanner = commands / "gitleaks"
    scanner.write_text(
        f"#!{sys.executable}\n"
        "import pathlib, sys\n"
        "assert sys.argv[1:-1] == ['dir', '--no-banner', '--redact']\n"
        "assert (pathlib.Path(sys.argv[-1]) / 'example.py').read_text() == 'staged content\\n'\n"
        "raise SystemExit(7)\n"
    )
    scanner.chmod(0o700)
    result = invoke(
        repository,
        "secrets",
        env={"PATH": str(commands) + os.pathsep + os.environ["PATH"]},
    )
    assert result.returncode == 7, result.stdout + result.stderr
    assert source.read_text() == "unstaged content\n"
