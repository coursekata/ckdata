"""Resolve Python configuration without changing AWS's environment."""

from __future__ import annotations

import builtins
import os
import sys
from collections.abc import Mapping, Sequence
from contextlib import contextmanager
from contextvars import ContextVar
from dataclasses import dataclass
from pathlib import Path
from typing import Annotated, Any, Literal

from pydantic import (
    AfterValidator,
    BaseModel,
    ConfigDict,
    Field,
    TypeAdapter,
    field_validator,
)


def _nonempty(value: str) -> str:
    if not value.strip() or any(ord(ch) < 32 for ch in value):
        raise ValueError("must be nonempty and contain no control characters")
    return value


Text = Annotated[str, AfterValidator(_nonempty)]
Timeout = Annotated[float, Field(gt=0, le=86400, allow_inf_nan=False)]


class Settings(BaseModel):
    model_config = ConfigDict(extra="forbid", strict=True)

    profile: Text | None = None
    region: Text | None = None
    workgroup: Text | None = None
    schema_: Text | None = Field(default=None, alias="schema")
    role_arn: Text | None = None
    output_location: Text | None = None
    sso_start_url: Text | None = None
    sso_region: Text | None = None
    sso_account_id: Text | None = None
    sso_role_name: Text | None = None
    sso_session: Text | None = None
    sso_registration_scopes: Text | None = None
    login: bool = False
    timeout: Timeout = 1800
    freshness: Any = None
    mode: Literal["explore", "audit", "publish"] = "explore"
    allow: dict[Text, Sequence[Text] | set[Text] | frozenset[Text]] = Field(
        default_factory=dict
    )
    verbose: bool = False
    saved_dir: str | Path | None = None
    snapshot_dir: str | Path | None = None

    @field_validator("allow")
    @classmethod
    def normalize_allow(cls, value):
        return {
            table: tuple(sorted(columns))
            if isinstance(columns, (set, frozenset))
            else tuple(columns)
            for table, columns in value.items()
        }

    @field_validator("freshness")
    @classmethod
    def validate_freshness(cls, value):
        from ._freshness import check_freshness

        check_freshness(value)
        return value


_SSO_FIELDS = (
    "start_url",
    "region",
    "account_id",
    "role_name",
    "session",
    "registration_scopes",
)
_CONNECTION_FIELDS = (
    "profile",
    "region",
    "workgroup",
    "schema",
    "role_arn",
    "output_location",
)
_defaults: dict[str, Any] = {}
_local: ContextVar[tuple[dict[str, Any], ...]] = ContextVar(
    "ckdata_options", default=()
)


def env_setting(name: str) -> str | None:
    return os.environ.get(f"CKDATA_{name}") or None


def current_options() -> dict[str, Any]:
    return {
        name: value
        for layer in (_defaults, *_local.get())
        for name, value in layer.items()
    }


def string(value: Any) -> str:
    return TypeAdapter(Text).validate_python(value, strict=True)


def boolean(value: Any) -> bool:
    return TypeAdapter(bool).validate_python(value, strict=True)


def seconds(value: Any) -> float:
    return TypeAdapter(Timeout).validate_python(value, strict=True)


def _validate(settings: Mapping[str, Any]) -> dict[str, Any]:
    validated = Settings(**settings)
    return {
        Settings.model_fields[name].alias or name: getattr(validated, name)
        for name in validated.model_fields_set
    }


def configure(**settings: Any) -> None:
    """Set application defaults; connection changes apply after lake_disconnect().

    Explicit connection arguments take precedence over these defaults, then
    CKDATA_* environment settings. Mode, freshness, directories,
    verbosity and timeout are read when collecting. Importing or configuring
    ckdata never connects or signs in.
    """
    _defaults.update(_validate(settings))


@contextmanager
def options(**settings: Any):
    """Temporarily override application defaults in this execution context."""
    token = _local.set((*_local.get(), _validate(settings)))
    try:
        yield
    finally:
        _local.reset(token)


def is_interactive() -> bool:
    return (
        hasattr(sys, "ps1")
        or getattr(builtins, "get_ipython", lambda: None)() is not None
    )


@dataclass(frozen=True)
class ConnectionSettings:
    values: dict[str, str | None]
    sso: dict[str, str | None]
    login: bool
    freshness: Any


def _auth_settings(layers, profile):
    # A profile selects a source as a whole. Keep each scope until that choice
    # is resolved, so lower-priority SSO fields cannot alter a selected identity.
    profile_layer = max(
        (index for index, layer in enumerate(layers) if "profile" in layer), default=-1
    )
    sso_fields = {
        name: (value, index)
        for index, layer in enumerate(layers)
        for name, value in layer.items()
        if name.startswith("sso_")
    }
    sso_layer = max(
        (index for value, index in sso_fields.values() if value is not None), default=-1
    )
    explicit_profile = profile_layer >= 0 and profile_layer >= sso_layer
    sso = {}
    for name in _SSO_FIELDS:
        key = f"sso_{name}"
        source = sso_fields.get(key)
        if source is not None and source[1] >= profile_layer:
            value = source[0]
        else:
            value = None if explicit_profile else env_setting(f"SSO_{name.upper()}")
        if value is not None:
            sso[name] = string(value)
    if sso and not explicit_profile:
        sso["fallback_profile"] = profile
        profile = None
    return profile, sso


def connection_settings(overrides: Mapping[str, Any]) -> ConnectionSettings:
    permitted = {
        *_CONNECTION_FIELDS,
        *(f"sso_{name}" for name in _SSO_FIELDS),
        "login",
        "freshness",
    }
    if unknown := overrides.keys() - permitted:
        raise TypeError(f"unknown connection setting: {', '.join(sorted(unknown))}")
    defaults = current_options()
    overrides = _validate(overrides)
    settings = defaults | overrides
    values = {
        name: settings.get(name, env_setting(name.upper()))
        for name in _CONNECTION_FIELDS
    }
    values["workgroup"] = string(values["workgroup"])
    values["schema"] = string(
        values["schema"] if values["schema"] is not None else "default"
    )
    values["profile"], sso = _auth_settings(
        (_defaults, *_local.get(), overrides), values["profile"]
    )
    login = boolean(settings.get("login", is_interactive()))
    return ConnectionSettings(values, sso, login, overrides.get("freshness"))
