"""Python settings preserve the R/AWS identity-selection contract."""

import os

import ckdata
import pytest
from ckdata._settings import connection_settings


def resolve(**overrides):
    return connection_settings({"workgroup": "synthetic", **overrides})


def test_legacy_environment_does_not_configure_a_new_python_connection(monkeypatch):
    monkeypatch.setenv("CK_DATALAKE_WORKGROUP", "legacy-workgroup")
    monkeypatch.setenv("CK_DATALAKE_INTERNAL_PROFILE", "legacy-profile")
    monkeypatch.setenv("CK_DATALAKE_SCHEMA", "legacy-schema")
    with pytest.raises(ValueError):
        connection_settings({})
    monkeypatch.setenv("CKDATA_WORKGROUP", "canonical")
    selected = connection_settings({})
    assert selected.values["workgroup"] == "canonical"
    assert selected.values["profile"] is None
    assert selected.values["schema"] == "default"
    monkeypatch.setenv("CKDATA_WORKGROUP", "")
    with pytest.raises(ValueError):
        connection_settings({})


def test_connection_arguments_override_python_options_then_environment(monkeypatch):
    monkeypatch.setenv("CKDATA_WORKGROUP", "environment")
    ckdata.configure(workgroup="application")
    assert connection_settings({}).values["workgroup"] == "application"
    with ckdata.options(workgroup="local"):
        assert connection_settings({}).values["workgroup"] == "local"
        assert (
            connection_settings({"workgroup": "argument"}).values["workgroup"]
            == "argument"
        )
    assert connection_settings({}).values["workgroup"] == "application"


def test_explicit_profile_ignores_sso_environment_and_explicit_none_bypasses_profile(
    monkeypatch,
):
    monkeypatch.setenv("CKDATA_PROFILE", "ambient-profile")
    monkeypatch.setenv("CKDATA_SSO_ACCOUNT_ID", "222222222222")
    monkeypatch.setenv("AWS_ACCESS_KEY_ID", "invented-not-a-real-key")
    before = dict(os.environ)
    selected = resolve(profile="argument-profile")
    assert selected.values["profile"] == "argument-profile"
    assert selected.sso == {}
    assert resolve(profile=None).values["profile"] is None
    assert resolve(profile=None).sso == {}
    assert dict(os.environ) == before


def test_direct_sso_arguments_override_environment_and_fill_from_one_fallback_profile(
    monkeypatch,
):
    monkeypatch.setenv("CKDATA_PROFILE", "fallback")
    monkeypatch.setenv("CKDATA_SSO_REGION", "us-east-1")
    monkeypatch.setenv("CKDATA_SSO_ROLE_NAME", "environment-role")
    chosen = resolve(sso_role_name="argument-role")
    assert chosen.values["profile"] is None
    assert chosen.sso == {
        "region": "us-east-1",
        "role_name": "argument-role",
        "fallback_profile": "fallback",
    }
    chosen = resolve(profile="explicit", sso_role_name="argument-role")
    assert chosen.values["profile"] == "explicit"
    assert chosen.sso == {"role_name": "argument-role"}
    assert resolve(sso_role_name=None).sso == {
        "region": "us-east-1",
        "fallback_profile": "fallback",
    }


def test_no_package_authentication_settings_leaves_aws_discovery_to_rust(monkeypatch):
    monkeypatch.setenv("AWS_PROFILE", "standard-aws-profile")
    monkeypatch.setenv("AWS_REGION", "us-west-1")
    chosen = resolve()
    assert chosen.values == {
        "workgroup": "synthetic",
        "schema": "default",
        "profile": None,
        "region": None,
        "role_arn": None,
        "output_location": None,
    }
    assert chosen.sso == {}


def test_script_login_defaults_off_and_explicit_setting_wins(monkeypatch):
    monkeypatch.setattr("ckdata._settings.is_interactive", lambda: False)
    assert resolve().login is False
    ckdata.configure(login=True)
    assert resolve().login is True
    assert resolve(login=False).login is False


def test_profile_and_sso_source_selection_respects_argument_over_application_priority(
    monkeypatch,
):
    monkeypatch.setenv("CKDATA_SSO_REGION", "us-west-1")
    ckdata.configure(sso_account_id="111111111111", sso_role_name="application-role")
    selected = resolve(profile="explicit-profile")
    assert selected.values["profile"] == "explicit-profile"
    assert selected.sso == {}
    selected = resolve(profile="explicit-profile", sso_role_name="explicit-role")
    assert selected.sso == {"role_name": "explicit-role"}
    ckdata.configure(profile="application-profile")
    selected = resolve(sso_role_name="explicit-role")
    assert selected.values["profile"] is None
    assert selected.sso == {
        "account_id": "111111111111",
        "region": "us-west-1",
        "role_name": "explicit-role",
        "fallback_profile": "application-profile",
    }


def test_context_profile_suppresses_lower_sso_and_retains_same_context_overrides(
    monkeypatch,
):
    monkeypatch.setenv("CKDATA_SSO_REGION", "us-west-1")
    ckdata.configure(sso_account_id="111111111111", sso_role_name="application-role")
    with ckdata.options(profile="context-profile"):
        selected = resolve()
        assert selected.values["profile"] == "context-profile"
        assert selected.sso == {}
    with ckdata.options(profile="context-profile", sso_role_name="context-role"):
        selected = resolve()
        assert selected.values["profile"] == "context-profile"
        assert selected.sso == {"role_name": "context-role"}


def test_nested_authentication_contexts_respect_priority_and_restore_the_outer_source(
    monkeypatch,
):
    monkeypatch.setenv("CKDATA_SSO_REGION", "us-west-1")
    ckdata.configure(profile="application-profile")
    with ckdata.options(sso_account_id="111111111111", sso_role_name="outer-role"):
        outer = resolve()
        assert outer.values["profile"] is None
        assert outer.sso == {
            "account_id": "111111111111",
            "role_name": "outer-role",
            "region": "us-west-1",
            "fallback_profile": "application-profile",
        }
        with ckdata.options(profile="inner-profile"):
            assert resolve().values["profile"] == "inner-profile"
            assert resolve().sso == {}
            assert resolve(sso_role_name="argument-role").sso == {
                "role_name": "argument-role",
                "region": "us-west-1",
                "fallback_profile": "inner-profile",
            }
        assert resolve() == outer


def test_clearing_a_context_sso_override_restores_profile_authority(monkeypatch):
    monkeypatch.setenv("CKDATA_SSO_ACCOUNT_ID", "222222222222")
    ckdata.configure(profile="application-profile")
    with ckdata.options(sso_role_name="context-role"):
        selected = resolve(sso_role_name=None)
        assert selected.values["profile"] == "application-profile"
        assert selected.sso == {}


@pytest.mark.parametrize(
    "settings,error",
    [
        ({"workgroup": ""}, ValueError),
        ({"workgroup": None}, ValueError),
        ({"timeout": 0}, ValueError),
        ({"timeout": float("nan")}, ValueError),
        ({"login": "false"}, ValueError),
        ({"mode": "unknown"}, ValueError),
        ({"allow": {"table": "column"}}, ValueError),
        ({"unknown": True}, ValueError),
    ],
)
def test_invalid_configuration_fails_before_authentication(settings, error):
    with pytest.raises(error):
        if "workgroup" in settings:
            connection_settings(settings)
        else:
            ckdata.configure(**settings)


def test_application_freshness_is_read_at_collection_not_captured_by_connection():
    policy = ckdata.daily_at("07:00", "America/Los_Angeles")
    ckdata.configure(freshness=policy)
    assert resolve().freshness is None
    assert resolve(freshness=policy).freshness == policy
