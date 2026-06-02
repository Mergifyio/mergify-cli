from __future__ import annotations

import typing

from click import testing

from mergify_cli.ci import cli as ci_cli
from mergify_cli.exit_codes import ExitCode


if typing.TYPE_CHECKING:
    import pathlib

    import pytest


def test_scopes_empty_mergify_config_env_uses_autodetection(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """When MERGIFY_CONFIG_PATH is set but empty, the config should be auto-detected."""
    config_file = tmp_path / ".mergify.yml"
    config_file.write_text("scopes:\n  source:\n    manual:\n")

    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("MERGIFY_CONFIG_PATH", "")

    runner = testing.CliRunner()
    result = runner.invoke(ci_cli.scopes, ["--base", "old", "--head", "new"])

    # The command found the auto-detected config and ran; source is manual so
    # ScopesError is raised -> CONFIGURATION_ERROR exit code.
    assert result.exit_code == ExitCode.CONFIGURATION_ERROR
    assert "source `manual` has been set" in result.output
