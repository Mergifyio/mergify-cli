from __future__ import annotations

from unittest import mock

from click import testing
import httpx
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


def test_clirunner_translates_mergify_error_to_exit_code() -> None:
    """CliRunner must see the typed exit code when MergifyError is raised."""
    import click

    @click.command()
    def fail_cmd() -> None:
        raise utils.MergifyError(
            "exploded",
            exit_code=ExitCode.CONFIGURATION_ERROR,
        )

    runner = testing.CliRunner()
    result = runner.invoke(fail_cmd, [])
    assert result.exit_code == ExitCode.CONFIGURATION_ERROR, result.output
    assert "error: exploded" in (result.output or "")


def test_clirunner_mergify_error_default_is_generic() -> None:
    """Default MergifyError exit code is GENERIC_ERROR (1)."""
    import click

    @click.command()
    def fail_cmd() -> None:
        raise utils.MergifyError("plain")

    runner = testing.CliRunner()
    result = runner.invoke(fail_cmd, [])
    assert result.exit_code == ExitCode.GENERIC_ERROR, result.output


def test_cli_connect_timeout_shows_clean_message(
    capsys: pytest.CaptureFixture[str],
) -> None:
    """httpx.ConnectTimeout to GitHub produces a friendly message, no traceback."""
    request = httpx.Request("GET", "https://api.github.com/user")
    error = httpx.ConnectTimeout("timeout", request=request)
    with (
        mock.patch.object(cli_mod, "cli", side_effect=error),
        pytest.raises(SystemExit) as exc_info,
    ):
        cli_mod.main()

    assert exc_info.value.code == ExitCode.GITHUB_API_ERROR
    out = capsys.readouterr().out
    assert "timed out" in out
    assert "https://api.github.com/user" in out
    assert "Traceback" not in out
    assert "ConnectTimeout" not in out


def test_cli_connect_timeout_to_mergify_api(
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Timeouts against the Mergify API map to MERGIFY_API_ERROR."""
    request = httpx.Request("GET", f"{utils.get_mergify_api_url()}/v1/foo")
    error = httpx.ConnectTimeout("timeout", request=request)
    with (
        mock.patch.object(cli_mod, "cli", side_effect=error),
        pytest.raises(SystemExit) as exc_info,
    ):
        cli_mod.main()

    assert exc_info.value.code == ExitCode.MERGIFY_API_ERROR
    assert "timed out" in capsys.readouterr().out


def test_cli_connect_error_shows_clean_message(
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Non-timeout transport errors also get a friendly message."""
    request = httpx.Request("GET", "https://api.github.com/user")
    error = httpx.ConnectError("connection refused", request=request)
    with (
        mock.patch.object(cli_mod, "cli", side_effect=error),
        pytest.raises(SystemExit) as exc_info,
    ):
        cli_mod.main()

    assert exc_info.value.code == ExitCode.GITHUB_API_ERROR
    out = capsys.readouterr().out
    assert "network error" in out
    assert "connection refused" in out


def test_main_entrypoint_handles_mergify_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """main() invokes click in standalone mode; MergifyError from inside
    the CLI must cause SystemExit with the typed exit code."""
    import sys

    monkeypatch.setattr(sys, "argv", ["mergify"])

    import click

    @click.command()
    def fail_cmd() -> None:
        raise utils.MergifyError(
            "nope",
            exit_code=ExitCode.INVALID_STATE,
        )

    monkeypatch.setattr(cli_mod, "cli", fail_cmd)

    with pytest.raises(SystemExit) as exc_info:
        cli_mod.main()

    assert exc_info.value.code == ExitCode.INVALID_STATE
