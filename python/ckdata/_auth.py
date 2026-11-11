"""AWS CLI interaction on the Python thread; AWS providers own renewal."""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
import time
import warnings
from pathlib import Path

from ._errors import CkDataError, SignInError


def _aws_cli() -> str:
    aws = shutil.which("aws")
    if aws is None:
        raise SignInError(
            "Browser sign-in requires AWS CLI v2. Install it or authenticate separately and set login=False."
        )
    try:
        version = subprocess.run(
            [aws, "--version"], capture_output=True, text=True, check=True
        )
    except (OSError, subprocess.CalledProcessError) as error:
        raise SignInError("Could not run AWS CLI v2 for browser sign-in.") from error
    if not (version.stdout or version.stderr).startswith("aws-cli/2."):
        raise SignInError(
            "Browser sign-in requires AWS CLI v2; update the installed AWS CLI."
        )
    return aws


def _run_login(aws: str, target: dict, job, directory: str) -> None:
    arguments = [aws, "sso", "login", "--profile", target["profile"]]
    if target.get("session"):
        arguments.extend(["--sso-session", target["session"]])
    arguments.extend(["--no-cli-pager", "--no-cli-auto-prompt"])
    env = None
    if target.get("config") is not None:
        config = Path(directory) / "config"
        with config.open("x", encoding="utf-8") as stream:
            config.chmod(0o600)
            stream.write(target["config"])
        env = os.environ | {"AWS_CONFIG_FILE": str(config)}
    print(
        "Opening AWS SSO sign-in in your browser. Complete sign-in to continue; interrupt Python to cancel.",
        file=sys.stderr,
    )
    try:
        with subprocess.Popen(arguments, env=env) as process:
            try:
                while process.poll() is None:
                    if job.ready():
                        return
                    time.sleep(0.05)
                if process.returncode:
                    raise SignInError(
                        "AWS SSO sign-in did not complete. No connection retry was made."
                    )
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()
    except OSError as error:
        raise SignInError("Could not start AWS CLI sign-in.") from error


def _sso_login(target: dict, job) -> None:
    aws = _aws_cli()
    with tempfile.TemporaryDirectory(prefix="ckdata-sso-") as directory:
        _run_login(aws, target, job, directory)


def _cancel_and_drain(job, cause: BaseException) -> None:
    job.cancel()
    try:
        while not job.ready():
            time.sleep(0.02)
        job.result()
    except CkDataError as error:
        for name in ("cancellation", "remote_may_be_running"):
            setattr(cause, name, getattr(error, name, None))
        if getattr(error, "remote_may_be_running", False):
            warnings.warn(str(error), RuntimeWarning, stacklevel=3)
    except KeyboardInterrupt:
        warnings.warn(
            "Cancellation is still pending; the Athena query may still be running.",
            RuntimeWarning,
            stacklevel=3,
        )


def wait(job):
    """Service one native operation without resubmitting it after sign-in."""
    try:
        while not job.ready():
            if target := job.login_target():
                try:
                    _sso_login(target, job)
                except BaseException:
                    job.login_complete(False)
                    raise
                job.login_complete(True)
            time.sleep(0.02)
        return job.result()
    except (KeyboardInterrupt, SignInError) as error:
        _cancel_and_drain(job, error)
        raise
