from __future__ import annotations

import typing
from unittest import mock

from click import testing
from click.testing import CliRunner
from httpx import Response
import respx

from mergify_cli import cli as cli_mod
from mergify_cli.config.cli import config
from mergify_cli.exit_codes import ExitCode


if typing.TYPE_CHECKING:
    import pathlib

    import pytest


_MINIMAL_SCHEMA: dict[str, object] = {
    "$schema": "http://json-schema.org/draft-07/schema#",
    "type": "object",
    "properties": {
        "pull_request_rules": {
            "type": "array",
        },
    },
    "additionalProperties": False,
}

_SCHEMA_URL = "https://docs.mergify.com/mergify-configuration-schema.json"
_PR_URL = "https://github.com/owner/repo/pull/42"


def _write_config(tmp_path: pathlib.Path, content: str) -> str:
    config_file = tmp_path / ".mergify.yml"
    config_file.write_text(content)
    return str(config_file)


def test_valid_config(tmp_path: pathlib.Path) -> None:
    config_path = _write_config(tmp_path, "pull_request_rules: []\n")

    with respx.mock:
        respx.get(_SCHEMA_URL).mock(
            return_value=Response(200, json=_MINIMAL_SCHEMA),
        )

        result = CliRunner().invoke(
            config,
            ["--config-file", config_path, "validate"],
        )
        assert result.exit_code == 0, result.output
        assert "is valid" in result.output


def test_invalid_config(tmp_path: pathlib.Path) -> None:
    config_path = _write_config(tmp_path, "unknown_key: true\n")

    with respx.mock:
        respx.get(_SCHEMA_URL).mock(
            return_value=Response(200, json=_MINIMAL_SCHEMA),
        )

        result = CliRunner().invoke(
            config,
            ["--config-file", config_path, "validate"],
        )
        assert result.exit_code == ExitCode.CONFIGURATION_ERROR, result.output
        assert "error" in result.output.lower()


def test_invalid_yaml(tmp_path: pathlib.Path) -> None:
    config_path = _write_config(tmp_path, ":\n  - :\n    bad yaml {{{\n")

    result = CliRunner().invoke(
        config,
        ["--config-file", config_path, "validate"],
    )
    assert result.exit_code != 0
    assert "Invalid YAML" in result.output


def test_non_mapping_yaml(tmp_path: pathlib.Path) -> None:
    config_path = _write_config(tmp_path, "- item1\n- item2\n")

    result = CliRunner().invoke(
        config,
        ["--config-file", config_path, "validate"],
    )
    assert result.exit_code != 0
    assert "mapping" in result.output.lower()


def test_config_not_found() -> None:
    result = CliRunner().invoke(
        config,
        ["--config-file", "/nonexistent/.mergify.yml", "validate"],
    )
    assert result.exit_code != 0
    assert "not found" in result.output.lower()


def test_auto_detect_config(tmp_path: pathlib.Path) -> None:
    config_file = tmp_path / ".mergify.yml"
    config_file.write_text("pull_request_rules: []\n")

    with (
        respx.mock,
        mock.patch(
            "mergify_cli.config.cli.get_mergify_config_path",
            return_value=str(config_file),
        ),
    ):
        respx.get(_SCHEMA_URL).mock(
            return_value=Response(200, json=_MINIMAL_SCHEMA),
        )

        result = CliRunner().invoke(config, ["validate"])
        assert result.exit_code == 0, result.output
        assert "is valid" in result.output


def test_auto_detect_not_found() -> None:
    with mock.patch(
        "mergify_cli.config.cli.get_mergify_config_path",
        return_value=None,
    ):
        result = CliRunner().invoke(config, ["validate"])
        assert result.exit_code != 0
        assert "not found" in result.output.lower()


def test_schema_fetch_failure(tmp_path: pathlib.Path) -> None:
    config_path = _write_config(tmp_path, "pull_request_rules: []\n")

    with respx.mock:
        respx.get(_SCHEMA_URL).mock(
            return_value=Response(500),
        )

        result = CliRunner().invoke(
            config,
            ["--config-file", config_path, "validate"],
        )
        assert result.exit_code != 0
        assert "Failed to fetch" in result.output


def test_empty_config(tmp_path: pathlib.Path) -> None:
    config_path = _write_config(tmp_path, "")

    with respx.mock:
        respx.get(_SCHEMA_URL).mock(
            return_value=Response(200, json=_MINIMAL_SCHEMA),
        )

        result = CliRunner().invoke(
            config,
            ["--config-file", config_path, "validate"],
        )
        assert result.exit_code == 0, result.output
        assert "is valid" in result.output


def test_simulate_pr(tmp_path: pathlib.Path) -> None:
    config_path = _write_config(tmp_path, "pull_request_rules: []\n")

    with respx.mock(base_url="https://api.mergify.com") as rsp:
        rsp.post("/v1/repos/owner/repo/pulls/42/simulator").mock(
            return_value=Response(
                200,
                json={
                    "title": "The configuration is valid",
                    "summary": "No actions will be triggered",
                },
            ),
        )

        result = CliRunner().invoke(
            config,
            [
                "--config-file",
                config_path,
                "simulate",
                _PR_URL,
                "--token",
                "test-token",
            ],
        )
        assert result.exit_code == 0, result.output
        assert "The configuration is valid" in result.output
        assert "No actions will be triggered" in result.output


def test_simulate_invalid_pr_url() -> None:
    result = CliRunner().invoke(
        config,
        ["simulate", "not-a-url", "--token", "test-token"],
    )
    assert result.exit_code != 0
    assert "Invalid pull request URL" in result.output


def test_simulate_api_failure(tmp_path: pathlib.Path) -> None:
    config_path = _write_config(tmp_path, "pull_request_rules: []\n")

    with respx.mock(base_url="https://api.mergify.com") as rsp:
        rsp.post("/v1/repos/owner/repo/pulls/42/simulator").mock(
            return_value=Response(500),
        )

        result = CliRunner().invoke(
            config,
            [
                "--config-file",
                config_path,
                "simulate",
                _PR_URL,
                "--token",
                "test-token",
            ],
        )
        assert result.exit_code != 0
        assert "Traceback" not in result.output


def test_simulate_config_not_found() -> None:
    result = CliRunner().invoke(
        config,
        [
            "--config-file",
            "/nonexistent/.mergify.yml",
            "simulate",
            _PR_URL,
            "--token",
            "test-token",
        ],
    )
    assert result.exit_code != 0
    assert "not found" in result.output.lower()


def test_config_not_found_exits_configuration_error(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """config validate with no config file available exits CONFIGURATION_ERROR."""
    monkeypatch.chdir(tmp_path)  # no .mergify.yml anywhere
    runner = testing.CliRunner()
    result = runner.invoke(cli_mod.cli, ["config", "validate"])
    assert result.exit_code == ExitCode.CONFIGURATION_ERROR, result.output


def test_config_invalid_yaml_exits_configuration_error(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """config validate with invalid YAML exits CONFIGURATION_ERROR."""
    cfg = tmp_path / ".mergify.yml"
    cfg.write_text("not: valid: yaml: [")
    monkeypatch.chdir(tmp_path)
    runner = testing.CliRunner()
    result = runner.invoke(cli_mod.cli, ["config", "validate"])
    assert result.exit_code == ExitCode.CONFIGURATION_ERROR, result.output


def test_config_simulate_invalid_url_exits_2(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """config simulate with a non-PR URL exits 2 (click.BadParameter)."""
    cfg = tmp_path / ".mergify.yml"
    cfg.write_text("pull_request_rules: []\n")
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("MERGIFY_TOKEN", "fake")
    runner = testing.CliRunner()
    result = runner.invoke(
        cli_mod.cli,
        ["config", "simulate", "https://example.com/not-a-pr"],
    )
    assert result.exit_code == 2, result.output
