from __future__ import annotations

from unittest import mock

from click import testing
import pytest

from mergify_cli import cli as cli_mod
from mergify_cli import utils
from mergify_cli.exit_codes import ExitCode


def test_cli_command_error_shows_clean_message() -> None:
    """Test that CommandError produces a clean error message, not a traceback."""
    error = utils.CommandError(
        ("git", "pull", "--rebase", "origin", "main"),
        1,
        b"CONFLICT (content): Merge conflict in file.txt",
    )
    with (
        mock.patch.object(cli_mod, "cli", side_effect=error),
        pytest.raises(SystemExit) as exc_info,
    ):
        cli_mod.main()

    assert exc_info.value.code == ExitCode.GENERIC_ERROR


def test_cli_shows_help_by_default() -> None:
    """Test that running `mergify` without arguments shows help."""
    runner = testing.CliRunner()
    result = runner.invoke(cli_mod.cli, [])

    assert result.exit_code == 0, result.output
    assert "Usage:" in result.output
    assert "Options:" in result.output
    assert "--help" in result.output
    assert "stack*" not in result.output
    assert "stack" in result.output
