"""Cross-command contract tests for exit codes.

Each parametrize entry corresponds to a row in docs/exit-codes.md.
Adding a failure mode to the CLI means adding a row here AND in
docs/exit-codes.md. They stay in lockstep.
"""

from __future__ import annotations

import typing

from click import testing
import pytest

from mergify_cli import cli as cli_mod
from mergify_cli.exit_codes import ExitCode


if typing.TYPE_CHECKING:
    from collections.abc import Callable
    import pathlib


@pytest.mark.parametrize(
    ("setup", "args", "expected_exit"),
    [
        pytest.param(
            lambda tmp_path, monkeypatch: monkeypatch.chdir(tmp_path),
            ["config", "validate"],
            ExitCode.CONFIGURATION_ERROR,
            id="config-validate-missing-file",
        ),
        pytest.param(
            lambda tmp_path, monkeypatch: _write_and_cd(
                tmp_path,
                monkeypatch,
                "not: valid: [",
            ),
            ["config", "validate"],
            ExitCode.CONFIGURATION_ERROR,
            id="config-validate-invalid-yaml",
        ),
        pytest.param(
            lambda tmp_path, monkeypatch: _prepare_simulate_env(tmp_path, monkeypatch),  # noqa: PLW0108
            ["config", "simulate", "https://example.com/not-a-pr"],
            2,
            id="config-simulate-bad-url",
        ),
        pytest.param(
            lambda tmp_path, monkeypatch: monkeypatch.chdir(tmp_path),
            ["ci", "scopes"],
            ExitCode.CONFIGURATION_ERROR,
            id="ci-scopes-missing-config",
        ),
        pytest.param(
            lambda _tmp_path, monkeypatch: _clear_mq_env(monkeypatch),
            ["ci", "queue-info"],
            ExitCode.INVALID_STATE,
            id="ci-queue-info-outside-mq",
        ),
    ],
)
def test_exit_code_contract(
    setup: Callable[[pathlib.Path, pytest.MonkeyPatch], None],
    args: list[str],
    expected_exit: int,
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Each entry asserts (args under setup) -> expected_exit."""
    setup(tmp_path, monkeypatch)
    runner = testing.CliRunner()
    result = runner.invoke(cli_mod.cli, args)
    assert result.exit_code == expected_exit, (
        f"expected {expected_exit}, got {result.exit_code}\noutput: {result.output}"
    )


def _write_and_cd(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
    yaml_content: str,
) -> None:
    (tmp_path / ".mergify.yml").write_text(yaml_content)
    monkeypatch.chdir(tmp_path)


def _prepare_simulate_env(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    (tmp_path / ".mergify.yml").write_text("pull_request_rules: []\n")
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("MERGIFY_TOKEN", "fake")


def _clear_mq_env(monkeypatch: pytest.MonkeyPatch) -> None:
    for var in [
        "GITHUB_EVENT_NAME",
        "GITHUB_EVENT_PATH",
        "GITHUB_HEAD_REF",
        "GITHUB_BASE_REF",
        "MERGIFY_QUEUE_BATCH_ID",
    ]:
        monkeypatch.delenv(var, raising=False)
