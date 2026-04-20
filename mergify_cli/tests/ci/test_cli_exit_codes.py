from __future__ import annotations

import typing

from click import testing

from mergify_cli import cli as cli_mod
from mergify_cli.exit_codes import ExitCode


if typing.TYPE_CHECKING:
    import pathlib

    import pytest


def test_ci_scopes_missing_config_exits_configuration_error(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.chdir(tmp_path)
    runner = testing.CliRunner()
    result = runner.invoke(cli_mod.cli, ["ci", "scopes"])
    assert result.exit_code == ExitCode.CONFIGURATION_ERROR, result.output


def test_ci_scopes_nonexistent_config_path_exits_configuration_error(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.chdir(tmp_path)
    runner = testing.CliRunner()
    result = runner.invoke(
        cli_mod.cli,
        ["ci", "scopes", "--config", str(tmp_path / "nope.yml")],
    )
    assert result.exit_code == ExitCode.CONFIGURATION_ERROR, result.output


def test_ci_queue_info_outside_merge_queue_exits_invalid_state(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    for var in [
        "GITHUB_EVENT_NAME",
        "GITHUB_EVENT_PATH",
        "GITHUB_HEAD_REF",
        "GITHUB_BASE_REF",
        "MERGIFY_QUEUE_BATCH_ID",
    ]:
        monkeypatch.delenv(var, raising=False)
    runner = testing.CliRunner()
    result = runner.invoke(cli_mod.cli, ["ci", "queue-info"])
    assert result.exit_code == ExitCode.INVALID_STATE, result.output
