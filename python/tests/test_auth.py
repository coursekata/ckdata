"""Exercise sign-in and interrupts without starting AWS or opening a browser."""

import os
import stat
import time
from pathlib import Path
from types import SimpleNamespace

import ckdata
import pytest
from ckdata import _auth, _native


class LoginJob:
    def __init__(self):
        self.completed = []
        self.cancelled = False
        self.consumed = 0

    def ready(self):
        return bool(self.completed)

    def login_target(self):
        return {"profile": "synthetic", "session": None, "config": None}

    def login_complete(self, success):
        self.completed.append(success)

    def cancel(self):
        self.cancelled = True

    def result(self):
        self.consumed += 1
        if self.cancelled:
            error = ckdata.CkDataError("synthetic cancellation")
            error.cancellation = "requested"
            error.remote_may_be_running = False
            raise error
        return "same native operation"


def test_sign_in_resumes_the_same_operation_once(monkeypatch):
    job = LoginJob()
    requests = []
    monkeypatch.setattr(
        _auth, "_sso_login", lambda target, active: requests.append((target, active))
    )
    assert _auth.wait(job) == "same native operation"
    assert [active for _, active in requests] == [job]
    assert job.completed == [True]
    assert job.consumed == 1


def test_failed_login_stops_and_drains_without_a_retry(monkeypatch):
    job = LoginJob()

    def fail(*args):
        raise ckdata.SignInError("synthetic sign-in failure")

    monkeypatch.setattr(_auth, "_sso_login", fail)
    with pytest.raises(ckdata.SignInError) as failure:
        _auth.wait(job)
    assert job.completed == [False]
    assert job.cancelled
    assert job.consumed == 1
    assert failure.value.cancellation == "requested"


def test_disabled_login_and_permission_errors_never_start_browser(monkeypatch):
    def unexpected(*args):
        pytest.fail("sign-in must not start")

    monkeypatch.setattr(_auth, "_sso_login", unexpected)
    for error in (
        ckdata.CkDataError("AccessDenied"),
        ckdata.CkDataError("SSO sign-in required"),
    ):
        job = LoginJob()
        job.completed.append(False)
        monkeypatch.setattr(
            job, "result", lambda error=error: (_ for _ in ()).throw(error)
        )
        with pytest.raises(ckdata.CkDataError) as caught:
            _auth.wait(job)
        assert caught.value is error


def test_cli_uses_private_config_without_changing_parent_environment(
    monkeypatch, tmp_path
):
    target = {
        "profile": "ckdata-direct",
        "session": "session",
        "config": "[profile ckdata-direct]\nsso_session = session\n",
    }
    invocations = []
    before = dict(os.environ)

    class Process:
        returncode = 0

        def __init__(self, arguments, *, env):
            path = Path(env["AWS_CONFIG_FILE"])
            assert path.read_text() == target["config"]
            if os.name != "nt":
                assert stat.S_IMODE(path.stat().st_mode) == 0o600
            invocations.append((arguments, path))

        def poll(self):
            return 0

        def __enter__(self):
            return self

        def __exit__(self, *args):
            pass

    monkeypatch.setattr(_auth, "_aws_cli", lambda: "invented-aws")
    monkeypatch.setattr(_auth.subprocess, "Popen", Process)
    _auth._sso_login(target, LoginJob())
    arguments, config = invocations[0]
    assert arguments == [
        "invented-aws",
        "sso",
        "login",
        "--profile",
        "ckdata-direct",
        "--sso-session",
        "session",
        "--no-cli-pager",
        "--no-cli-auto-prompt",
    ]
    assert not config.exists()
    assert dict(os.environ) == before


@pytest.mark.parametrize("version", ["aws-cli/1.0", "not AWS CLI"])
def test_sign_in_requires_cli_version_two(monkeypatch, version):
    monkeypatch.setattr(_auth.shutil, "which", lambda name: "invented-aws")
    monkeypatch.setattr(
        _auth.subprocess,
        "run",
        lambda *args, **kwargs: SimpleNamespace(stdout=version, stderr=""),
    )
    with pytest.raises(ckdata.SignInError, match="requires AWS CLI v2"):
        _auth._aws_cli()


def test_missing_cli_fails_with_installation_guidance(monkeypatch):
    monkeypatch.setattr(_auth.shutil, "which", lambda name: None)
    with pytest.raises(ckdata.SignInError, match="login=False"):
        _auth._aws_cli()


def test_interrupt_cancels_compiled_native_job_and_preserves_stop_status(monkeypatch):
    job = _native._pending_job()
    original_sleep = time.sleep
    interrupted = False

    def interrupt_once(seconds):
        nonlocal interrupted
        if not interrupted:
            interrupted = True
            raise KeyboardInterrupt
        original_sleep(seconds)

    monkeypatch.setattr(_auth.time, "sleep", interrupt_once)
    with pytest.raises(KeyboardInterrupt) as caught:
        _auth.wait(job)
    assert caught.value.cancellation == "requested"
    assert caught.value.remote_may_be_running is False
    with pytest.raises(RuntimeError, match="already consumed"):
        job.result()
