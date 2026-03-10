from __future__ import annotations

import typing
from unittest import mock


if typing.TYPE_CHECKING:
    import pathlib

from click.testing import CliRunner
from httpx import Response
import respx

from mergify_cli.config.cli import config


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
_SIMULATOR_URL = "https://api.mergify.com/v1/repos/owner/repo/configuration-simulator"

_ONLINE_ARGS = [
    "--online",
    "--token",
    "test-token",
    "--repository",
    "owner/repo",
]


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

        result = CliRunner().invoke(config, ["validate", "--config", config_path])
        assert result.exit_code == 0, result.output
        assert "is valid" in result.output


def test_invalid_config(tmp_path: pathlib.Path) -> None:
    config_path = _write_config(tmp_path, "unknown_key: true\n")

    with respx.mock:
        respx.get(_SCHEMA_URL).mock(
            return_value=Response(200, json=_MINIMAL_SCHEMA),
        )

        result = CliRunner().invoke(config, ["validate", "--config", config_path])
        assert result.exit_code == 1, result.output
        assert "error" in result.output.lower()


def test_invalid_yaml(tmp_path: pathlib.Path) -> None:
    config_path = _write_config(tmp_path, ":\n  - :\n    bad yaml {{{\n")

    result = CliRunner().invoke(config, ["validate", "--config", config_path])
    assert result.exit_code != 0
    assert "Invalid YAML" in result.output


def test_non_mapping_yaml(tmp_path: pathlib.Path) -> None:
    config_path = _write_config(tmp_path, "- item1\n- item2\n")

    result = CliRunner().invoke(config, ["validate", "--config", config_path])
    assert result.exit_code != 0
    assert "mapping" in result.output.lower()


def test_config_not_found() -> None:
    result = CliRunner().invoke(
        config,
        ["validate", "--config", "/nonexistent/.mergify.yml"],
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

        result = CliRunner().invoke(config, ["validate", "--config", config_path])
        assert result.exit_code != 0
        assert "Failed to fetch" in result.output


def test_empty_config(tmp_path: pathlib.Path) -> None:
    config_path = _write_config(tmp_path, "")

    with respx.mock:
        respx.get(_SCHEMA_URL).mock(
            return_value=Response(200, json=_MINIMAL_SCHEMA),
        )

        result = CliRunner().invoke(config, ["validate", "--config", config_path])
        assert result.exit_code == 0, result.output
        assert "is valid" in result.output


def test_online_valid(tmp_path: pathlib.Path) -> None:
    config_path = _write_config(tmp_path, "pull_request_rules: []\n")

    with respx.mock:
        respx.get(_SCHEMA_URL).mock(
            return_value=Response(200, json=_MINIMAL_SCHEMA),
        )
        respx.post(_SIMULATOR_URL).mock(
            return_value=Response(
                200,
                json={"message": "The configuration is valid"},
            ),
        )

        result = CliRunner().invoke(
            config,
            ["validate", "--config", config_path, *_ONLINE_ARGS],
        )
        assert result.exit_code == 0, result.output
        assert "is valid" in result.output
        assert "Simulator" in result.output


def test_online_simulator_error(tmp_path: pathlib.Path) -> None:
    config_path = _write_config(tmp_path, "pull_request_rules: []\n")

    with respx.mock:
        respx.get(_SCHEMA_URL).mock(
            return_value=Response(200, json=_MINIMAL_SCHEMA),
        )
        respx.post(_SIMULATOR_URL).mock(
            return_value=Response(
                422,
                json={"detail": "Invalid condition 'foo'"},
            ),
        )

        result = CliRunner().invoke(
            config,
            ["validate", "--config", config_path, *_ONLINE_ARGS],
        )
        assert result.exit_code == 1, result.output
        assert "failed simulator" in result.output.lower()


def test_online_missing_token(tmp_path: pathlib.Path) -> None:
    config_path = _write_config(tmp_path, "pull_request_rules: []\n")

    with respx.mock:
        respx.get(_SCHEMA_URL).mock(
            return_value=Response(200, json=_MINIMAL_SCHEMA),
        )

        result = CliRunner().invoke(
            config,
            [
                "validate",
                "--config",
                config_path,
                "--online",
                "--token",
                "",
                "--repository",
                "owner/repo",
            ],
        )
        assert result.exit_code != 0
        assert "token" in result.output.lower()


def test_online_missing_repository(tmp_path: pathlib.Path) -> None:
    config_path = _write_config(tmp_path, "pull_request_rules: []\n")

    with respx.mock:
        respx.get(_SCHEMA_URL).mock(
            return_value=Response(200, json=_MINIMAL_SCHEMA),
        )

        result = CliRunner().invoke(
            config,
            [
                "validate",
                "--config",
                config_path,
                "--online",
                "--token",
                "test-token",
                "--repository",
                "",
            ],
        )
        assert result.exit_code != 0
        assert "repository" in result.output.lower()
