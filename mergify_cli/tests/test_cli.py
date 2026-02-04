from __future__ import annotations

from click import testing

from mergify_cli import cli as cli_mod


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
