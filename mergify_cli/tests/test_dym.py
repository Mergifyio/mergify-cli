from __future__ import annotations

import click
from click import testing

from mergify_cli.dym import DYMGroup


def _make_group() -> click.Group:
    @click.group(cls=DYMGroup)
    def cli() -> None:
        pass

    @cli.command()
    def stack() -> None:
        pass

    @cli.command()
    def freeze() -> None:
        pass

    @cli.command()
    def config() -> None:
        pass

    return cli


def test_exact_command() -> None:
    runner = testing.CliRunner()
    result = runner.invoke(_make_group(), ["stack"])
    assert result.exit_code == 0


def test_typo_suggests_close_match() -> None:
    runner = testing.CliRunner()
    result = runner.invoke(_make_group(), ["stac"])
    assert result.exit_code != 0
    assert "Did you mean" in result.output
    assert "'stack'" in result.output


def test_typo_no_match() -> None:
    runner = testing.CliRunner()
    result = runner.invoke(_make_group(), ["zzzzz"])
    assert result.exit_code != 0
    assert "No such command" in result.output
    assert "Did you mean" not in result.output


def test_multiple_suggestions() -> None:
    @click.group(cls=DYMGroup)
    def cli() -> None:
        pass

    @cli.command()
    def start() -> None:
        pass

    @cli.command()
    def status() -> None:
        pass

    @cli.command()
    def stop() -> None:
        pass

    runner = testing.CliRunner()
    result = runner.invoke(cli, ["stat"])
    assert result.exit_code != 0
    assert "Did you mean" in result.output
    # At least one of start/status should be suggested
    assert "'start'" in result.output or "'status'" in result.output
