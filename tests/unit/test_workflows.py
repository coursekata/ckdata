"""Execute publication guards with invented manifests and fake GitHub commands."""

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


def test_publication_is_disabled_until_explicitly_ready():
    script = script_after("release.yml", "name: Require first-release readiness")
    for ready, expected in (("false", 1), ("true", 0)):
        result = run_script(script, env={"R_RELEASE_READY": ready})
        assert result.returncode == expected, result.stderr


def test_ci_success_must_belong_to_the_tagged_commit():
    script = script_after(
        "release.yml", "name: Require successful CI for the exact tagged commit"
    )
    with tempfile.TemporaryDirectory(prefix="ckdata-workflow-test-") as directory:
        directory = Path(directory)
        for name, body in {
            "git": (
                "import sys\n"
                'assert sys.argv[1:] == ["rev-parse", "HEAD"], sys.argv\n'
                'print("synthetic-tagged-commit")'
            ),
            "gh": (
                "import os, re, sys\n"
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
                'normalized = "".join(re.findall(r\'"[^"]*"|\\S\', selector))\n'
                "assert normalized == '.workflow_runs[]|select(.head_branch==\"main\")|.head_sha', selector\n"
                'print(os.environ["SYNTHETIC_CI_SHA"])'
            ),
        }.items():
            command = directory / name
            command.write_text("#!/usr/bin/env python3\n" + body + "\n")
            command.chmod(0o700)
        env = {
            "PATH": str(directory) + os.pathsep + os.environ["PATH"],
            "RUNNER_TEMP": str(directory),
            "GITHUB_REPOSITORY": "synthetic/client",
        }
        for commit, expected in (
            ("synthetic-other-commit", 1),
            ("synthetic-tagged-commit", 0),
        ):
            result = run_script(script, env={**env, "SYNTHETIC_CI_SHA": commit})
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


def test_os_release_does_not_replace_package_version():
    script = script_after(
        "verify_release.yml", "name: Check R-universe built for this Linux release"
    )
    extraction = "\n".join(script.splitlines()[:2])
    with tempfile.TemporaryDirectory(prefix="ckdata-os-release-test-") as directory:
        source = Path(directory) / "os-release"
        source.write_text('VERSION="26.04.1 LTS"\nVERSION_CODENAME="synthetic"\n')
        result = run_script(
            "VERSION=0.0.1\n"
            + extraction.replace("/etc/os-release", str(source))
            + '\nprintf "%s %s" "$VERSION" "$VERSION_CODENAME"'
        )
        assert result.returncode == 0, result.stderr
        assert result.stdout == "0.0.1 synthetic"
