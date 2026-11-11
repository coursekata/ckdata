"""Python conditions; native errors retain query and cancellation details."""

from ._native import Error as CkDataError


class SignInError(CkDataError):
    """The requested AWS SSO browser sign-in could not complete."""


class ColumnWarning(UserWarning):
    """A query uses a deprecated or unstable source column."""


class ColumnPolicyError(ValueError):
    """Publish mode would leave a query with no output columns."""


class SavedResultWarning(UserWarning):
    """A saved result could not be read or written."""


class StaleResultWarning(SavedResultWarning):
    """A refresh failed and an available stale result was returned."""


class SnapshotError(ValueError):
    """An existing snapshot is unreadable and has been preserved."""
