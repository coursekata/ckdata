"""Execute publication guards with invented manifests and fake GitHub commands."""

import json
import os
import shutil
import subprocess
import tempfile
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def script_after(filename, marker):
    workflow = (ROOT / ".github/workflows" / filename).read_text()
    block = workflow.split(marker, 1)[1].split("        run: |\n", 1)[1]
    lines = []
    for line in block.splitlines():
        if line and (not line.startswith("          ")):
            break
        lines.append(line[10:])
    return "\n".join(lines)


def run_script(script, *, env=None, cwd=ROOT):
    return subprocess.run(
        ["bash", "-e", "-c", script],
        cwd=cwd,
        env={**os.environ, **(env or {})},
        capture_output=True,
        text=True,
        timeout=10,
        check=False,
    )


def test_ci_rebuilds_when_a_change_affects_package_code(tmp_path, monkeypatch):
    script = script_after("ci.yml", "name: Identify changes that require builds")
    for variable in subprocess.check_output(
        ["git", "rev-parse", "--local-env-vars"], text=True
    ).splitlines():
        monkeypatch.delenv(variable, raising=False)

    def git(*args):
        return subprocess.check_output(
            [
                "git",
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                *args,
            ],
            cwd=tmp_path,
            text=True,
        ).strip()

    git("init", "--quiet", "--template=")
    git("commit", "--allow-empty", "-qm", "base")
    base = git("rev-parse", "HEAD")
    for path in (
        "README.md",
        "docs/images/example.svg",
        "core/tests/fixtures/README.MD",
        ".github/dependency_policy.md",
        "r/NEWS.md",
    ):
        target = tmp_path / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text("Example documentation\n")
        git("add", "--", path)
    git("commit", "-qm", "docs")
    docs = git("rev-parse", "HEAD")

    # Renaming an ignored file to a source path still requires a build.
    (tmp_path / "r/R").mkdir()
    git("mv", "r/NEWS.md", "r/R/example.R")
    git("commit", "-qm", "package source")
    mixed = git("rev-parse", "HEAD")

    output = tmp_path / "output"
    for start, end, expected in (
        (base, docs, "false"),
        (docs, mixed, "true"),
        ("", docs, "true"),
    ):
        output.write_text("")
        result = run_script(
            script,
            cwd=tmp_path,
            env={"BASE_SHA": start, "GITHUB_SHA": end, "GITHUB_OUTPUT": str(output)},
        )
        assert result.returncode == 0, result.stderr
        assert output.read_text() == f"rebuild={expected}\n"


def test_required_ci_rejects_unexpected_skips():
    script = script_after("ci.yml", "name: Assert the expected jobs succeeded")
    for rebuild, classification, quality, build, expected in (
        ("true", "success", "success", "success", 0),
        ("false", "success", "success", "skipped", 0),
        ("true", "success", "success", "skipped", 1),
        ("false", "success", "failure", "skipped", 1),
        ("false", "failure", "success", "skipped", 1),
    ):
        needs = {
            "changes": {"result": classification, "outputs": {"rebuild": rebuild}},
            "quality": {"result": quality},
            "r": {"result": build},
        }
        result = run_script(script, env={"NEEDS": json.dumps(needs)})
        assert result.returncode == expected, result.stderr


def test_publication_is_disabled_until_explicitly_ready():
    script = script_after("release.yml", "name: Require first-release readiness")
    for ready, expected in (("false", 1), ("true", 0)):
        result = run_script(script, env={"R_RELEASE_READY": ready})
        assert result.returncode == expected, result.stderr


def test_ci_success_must_belong_to_the_tagged_commit():
    script = script_after(
        "release.yml", "name: Require successful CI for the exact tagged commit"
    )
    jq = shutil.which("jq")
    assert jq is not None, "jq is required to execute the workflow's CI selector"
    with tempfile.TemporaryDirectory(prefix="ckdata-workflow-test-") as directory:
        directory = Path(directory)
        for name, body in {
            "git": (
                "import sys\n"
                'assert sys.argv[1:] == ["rev-parse", "HEAD"], sys.argv\n'
                'print("synthetic-tagged-commit")'
            ),
            "gh": (
                "import os, subprocess, sys\n"
                "from urllib.parse import parse_qs, urlsplit\n"
                "arguments = sys.argv[1:]\n"
                'assert arguments.pop(0) == "api", sys.argv\n'
                'arguments.remove("--paginate")\n'
                'selector_index = arguments.index("--jq")\n'
                "selector = arguments.pop(selector_index + 1)\n"
                "arguments.pop(selector_index)\n"
                "assert len(arguments) == 1, sys.argv\n"
                "endpoint = urlsplit(arguments[0])\n"
                'assert endpoint.path == "repos/synthetic/client/actions/workflows/ci.yml/runs", endpoint\n'
                "query = parse_qs(endpoint.query)\n"
                'for key, value in {"head_sha": "synthetic-tagged-commit", "event": "push", "status": "success"}.items():\n'
                "    assert query.get(key) == [value], query\n"
                'subprocess.run([os.environ["SYNTHETIC_JQ"], "-r", selector],\n'
                '    input=os.environ["SYNTHETIC_CI_RUNS"], text=True, check=True)'
            ),
        }.items():
            command = directory / name
            command.write_text("#!/usr/bin/env python3\n" + body + "\n")
            command.chmod(0o700)
        env = {
            "PATH": str(directory) + os.pathsep + os.environ["PATH"],
            "RUNNER_TEMP": str(directory),
            "GITHUB_REPOSITORY": "synthetic/client",
            "SYNTHETIC_JQ": jq,
        }
        for commit, expected in (
            ("synthetic-other-commit", 1),
            ("synthetic-tagged-commit", 0),
        ):
            runs = {
                "workflow_runs": [
                    {
                        "head_branch": "main",
                        "head_sha": commit,
                        "status": "completed",
                        "conclusion": "success",
                    },
                    {
                        "head_branch": "synthetic-feature",
                        "head_sha": "synthetic-tagged-commit",
                        "status": "completed",
                        "conclusion": "success",
                    },
                ]
            }
            result = run_script(
                script, env={**env, "SYNTHETIC_CI_RUNS": json.dumps(runs)}
            )
            assert result.returncode == expected, result.stderr


def test_tag_checks_every_manifest_and_main_ancestry():
    script = script_after(
        "release.yml", "name: Check the tag names the manifests' version"
    )
    with tempfile.TemporaryDirectory(prefix="ckdata-version-test-") as directory:
        directory = Path(directory)
        manifests = (
            "core/Cargo.toml",
            "python/Cargo.toml",
            "r/src/rust/Cargo.toml",
            "r/DESCRIPTION",
        )
        for relative in manifests:
            target = directory / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / relative, target)
        command = directory / "git"
        command.write_text(
            "#!/usr/bin/env python3\nimport os, sys\n"
            'assert sys.argv[1:] == ["merge-base", "--is-ancestor", "synthetic-commit", "origin/main"], sys.argv\n'
            'raise SystemExit(int(os.environ.get("SYNTHETIC_GIT_STATUS", "0")))\n'
        )
        command.chmod(0o700)
        with (ROOT / "core/Cargo.toml").open("rb") as manifest:
            version = tomllib.load(manifest)["package"]["version"]
        env = {
            "PATH": str(directory) + os.pathsep + os.environ["PATH"],
            "GITHUB_REF_NAME": f"v{version}",
            "GITHUB_SHA": "synthetic-commit",
        }
        result = run_script(script, env=env, cwd=directory)
        assert result.returncode == 0, result.stderr
        assert (
            run_script(
                script, env={**env, "SYNTHETIC_GIT_STATUS": "1"}, cwd=directory
            ).returncode
            != 0
        )
        for relative in manifests:
            target = directory / relative
            original = target.read_text()
            target.write_text(original.replace(version, version + "-mismatch", 1))
            result = run_script(script, env=env, cwd=directory)
            assert result.returncode != 0, relative
            target.write_text(original)
