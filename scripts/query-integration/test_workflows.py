"""Execute publication guards with invented manifests and fake GitHub commands."""

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def script_after(filename, marker):
    workflow = (ROOT / ".github/workflows" / filename).read_text()
    block = workflow.split(marker, 1)[1].split("        run: |\n", 1)[1]
    lines = []
    for line in block.splitlines():
        if line and not line.startswith("          "):
            break
        lines.append(line[10:])
    return "\n".join(lines)


class PublicationTests(unittest.TestCase):
    def run_script(self, script, *, env=None, cwd=ROOT):
        return subprocess.run(
            ["bash", "-e", "-c", script],
            cwd=cwd,
            env={**os.environ, **(env or {})},
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )

    def test_publication_is_disabled_until_explicitly_ready(self):
        script = script_after("release.yml", "name: Require first-release readiness")
        for ready, expected in (("false", 1), ("true", 0)):
            result = self.run_script(script, env={"R_RELEASE_READY": ready})
            self.assertEqual(result.returncode, expected, result.stderr)

    def test_ci_success_must_belong_to_the_tagged_commit(self):
        script = script_after(
            "release.yml", "name: Require successful CI for the exact tagged commit"
        )
        with tempfile.TemporaryDirectory(prefix="ckdata-workflow-test-") as directory:
            directory = Path(directory)
            for name, body in {
                "git": "printf '%s\\n' synthetic-tagged-commit",
                "gh": 'printf "%s\\n" "$SYNTHETIC_CI_SHA"',
            }.items():
                command = directory / name
                command.write_text("#!/bin/sh\n" + body + "\n")
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
                result = self.run_script(
                    script, env={**env, "SYNTHETIC_CI_SHA": commit}
                )
                self.assertEqual(result.returncode, expected, result.stderr)

    def test_tag_checks_every_manifest_and_main_ancestry(self):
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
            command.write_text('#!/bin/sh\nexit "${SYNTHETIC_GIT_STATUS:-0}"\n')
            command.chmod(0o700)
            env = {
                "PATH": str(directory) + os.pathsep + os.environ["PATH"],
                "GITHUB_REF_NAME": "v0.0.1",
                "GITHUB_SHA": "synthetic-commit",
            }
            self.assertEqual(
                self.run_script(script, env=env, cwd=directory).returncode, 0
            )
            self.assertNotEqual(
                self.run_script(
                    script, env={**env, "SYNTHETIC_GIT_STATUS": "1"}, cwd=directory
                ).returncode,
                0,
            )
            for relative in manifests:
                target = directory / relative
                original = target.read_text()
                target.write_text(original.replace("0.0.1", "0.0.2", 1))
                result = self.run_script(script, env=env, cwd=directory)
                self.assertNotEqual(result.returncode, 0, relative)
                target.write_text(original)

    def test_os_release_does_not_replace_package_version(self):
        script = script_after(
            "verify-release.yml", "name: Check R-universe built for this Linux release"
        )
        extraction = "\n".join(script.splitlines()[:2])
        with tempfile.TemporaryDirectory(prefix="ckdata-os-release-test-") as directory:
            source = Path(directory) / "os-release"
            source.write_text('VERSION="26.04.1 LTS"\nVERSION_CODENAME="synthetic"\n')
            result = self.run_script(
                "VERSION=0.0.1\n"
                + extraction.replace("/etc/os-release", str(source))
                + '\nprintf "%s %s" "$VERSION" "$VERSION_CODENAME"',
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, "0.0.1 synthetic")
