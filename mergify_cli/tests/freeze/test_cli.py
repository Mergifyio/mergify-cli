from __future__ import annotations

import datetime
import json

import click
from click.testing import CliRunner
from httpx import Response
import pytest
import respx

from mergify_cli.freeze.cli import freeze


FAKE_FREEZE = {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "reason": "Release prep",
    "start": "2099-06-19T08:00:00",
    "end": "2099-06-20T17:00:00",
    "timezone": "Europe/Paris",
    "matching_conditions": ["base=main"],
    "exclude_conditions": [],
}

FAKE_FREEZE_WITH_EXCLUDE = {
    **FAKE_FREEZE,
    "exclude_conditions": ["label=hotfix"],
}

BASE_ARGS = [
    "--token",
    "test-token",
    "--api-url",
    "https://api.mergify.com",
    "--repository",
    "owner/repo",
]


def test_list_empty() -> None:
    with respx.mock(base_url="https://api.mergify.com") as mock:
        mock.get("/v1/repos/owner/repo/scheduled_freeze").mock(
            return_value=Response(200, json={"scheduled_freezes": []}),
        )

        runner = CliRunner()
        result = runner.invoke(freeze, [*BASE_ARGS, "list"])
        assert result.exit_code == 0
        assert "No scheduled freezes found" in result.output


def test_list_with_freezes() -> None:
    with respx.mock(base_url="https://api.mergify.com") as mock:
        mock.get("/v1/repos/owner/repo/scheduled_freeze").mock(
            return_value=Response(
                200,
                json={"scheduled_freezes": [FAKE_FREEZE]},
            ),
        )

        runner = CliRunner()
        result = runner.invoke(freeze, [*BASE_ARGS, "list"])
        assert result.exit_code == 0
        assert "Release" in result.output
        assert "base=main" in result.output
        assert "scheduled" in result.output


def test_list_json() -> None:
    with respx.mock(base_url="https://api.mergify.com") as mock:
        mock.get("/v1/repos/owner/repo/scheduled_freeze").mock(
            return_value=Response(
                200,
                json={"scheduled_freezes": [FAKE_FREEZE]},
            ),
        )

        runner = CliRunner()
        result = runner.invoke(freeze, [*BASE_ARGS, "list", "--json"])
        assert result.exit_code == 0
        data = json.loads(result.output)
        assert len(data) == 1
        assert data[0]["reason"] == "Release prep"


def test_list_with_exclude_conditions() -> None:
    with respx.mock(base_url="https://api.mergify.com") as mock:
        mock.get("/v1/repos/owner/repo/scheduled_freeze").mock(
            return_value=Response(
                200,
                json={"scheduled_freezes": [FAKE_FREEZE_WITH_EXCLUDE]},
            ),
        )

        runner = CliRunner()
        result = runner.invoke(freeze, [*BASE_ARGS, "list"])
        assert result.exit_code == 0
        assert "exclude" in result.output


def test_list_json_with_exclude_conditions() -> None:
    with respx.mock(base_url="https://api.mergify.com") as mock:
        mock.get("/v1/repos/owner/repo/scheduled_freeze").mock(
            return_value=Response(
                200,
                json={"scheduled_freezes": [FAKE_FREEZE_WITH_EXCLUDE]},
            ),
        )

        runner = CliRunner()
        result = runner.invoke(freeze, [*BASE_ARGS, "list", "--json"])
        assert result.exit_code == 0
        data = json.loads(result.output)
        assert data[0]["exclude_conditions"] == ["label=hotfix"]


def test_create_minimal() -> None:
    with respx.mock(base_url="https://api.mergify.com") as mock:
        mock.post("/v1/repos/owner/repo/scheduled_freeze").mock(
            return_value=Response(201, json=FAKE_FREEZE),
        )

        runner = CliRunner()
        result = runner.invoke(
            freeze,
            [
                *BASE_ARGS,
                "create",
                "--reason",
                "Release prep",
                "--timezone",
                "Europe/Paris",
                "-c",
                "base=main",
            ],
        )
        assert result.exit_code == 0, result.output
        assert "Freeze created successfully" in result.output
        assert "Release prep" in result.output

        request = mock.calls.last.request
        body = json.loads(request.content)
        assert body["reason"] == "Release prep"
        assert body["timezone"] == "Europe/Paris"
        assert body["matching_conditions"] == ["base=main"]
        assert "start" not in body
        assert "end" not in body


def test_create_with_all_options() -> None:
    with respx.mock(base_url="https://api.mergify.com") as mock:
        mock.post("/v1/repos/owner/repo/scheduled_freeze").mock(
            return_value=Response(201, json=FAKE_FREEZE_WITH_EXCLUDE),
        )

        runner = CliRunner()
        result = runner.invoke(
            freeze,
            [
                *BASE_ARGS,
                "create",
                "--reason",
                "Release prep",
                "--timezone",
                "Europe/Paris",
                "-c",
                "base=main",
                "--start",
                "2099-06-19T08:00:00",
                "--end",
                "2099-06-20T17:00:00",
                "-e",
                "label=hotfix",
            ],
        )
        assert result.exit_code == 0, result.output

        request = mock.calls.last.request
        body = json.loads(request.content)
        assert body["start"] == "2099-06-19T08:00:00"
        assert body["end"] == "2099-06-20T17:00:00"
        assert body["exclude_conditions"] == ["label=hotfix"]


def test_create_multiple_conditions() -> None:
    with respx.mock(base_url="https://api.mergify.com") as mock:
        mock.post("/v1/repos/owner/repo/scheduled_freeze").mock(
            return_value=Response(201, json=FAKE_FREEZE),
        )

        runner = CliRunner()
        result = runner.invoke(
            freeze,
            [
                *BASE_ARGS,
                "create",
                "--reason",
                "Multi-branch freeze",
                "--timezone",
                "UTC",
                "-c",
                "base=main",
                "-c",
                "base=release",
            ],
        )
        assert result.exit_code == 0, result.output

        request = mock.calls.last.request
        body = json.loads(request.content)
        assert body["matching_conditions"] == ["base=main", "base=release"]


def test_create_missing_required() -> None:
    runner = CliRunner()
    result = runner.invoke(
        freeze,
        [*BASE_ARGS, "create", "--reason", "test"],
    )
    assert result.exit_code != 0


def test_update() -> None:
    freeze_id = "550e8400-e29b-41d4-a716-446655440000"
    updated_freeze = {**FAKE_FREEZE, "reason": "Updated reason"}

    with respx.mock(base_url="https://api.mergify.com") as mock:
        mock.patch(
            f"/v1/repos/owner/repo/scheduled_freeze/{freeze_id}",
        ).mock(
            return_value=Response(200, json=updated_freeze),
        )

        runner = CliRunner()
        result = runner.invoke(
            freeze,
            [
                *BASE_ARGS,
                "update",
                freeze_id,
                "--reason",
                "Updated reason",
                "--timezone",
                "Europe/Paris",
                "-c",
                "base=main",
            ],
        )
        assert result.exit_code == 0, result.output
        assert "Freeze updated successfully" in result.output
        assert "Updated reason" in result.output


def test_update_with_end() -> None:
    freeze_id = "550e8400-e29b-41d4-a716-446655440000"

    with respx.mock(base_url="https://api.mergify.com") as mock:
        mock.patch(
            f"/v1/repos/owner/repo/scheduled_freeze/{freeze_id}",
        ).mock(
            return_value=Response(200, json=FAKE_FREEZE),
        )

        runner = CliRunner()
        result = runner.invoke(
            freeze,
            [
                *BASE_ARGS,
                "update",
                freeze_id,
                "--reason",
                "Release prep",
                "--timezone",
                "Europe/Paris",
                "-c",
                "base=main",
                "--end",
                "2099-12-31T23:59:59",
            ],
        )
        assert result.exit_code == 0, result.output

        request = mock.calls.last.request
        body = json.loads(request.content)
        assert body["end"] == "2099-12-31T23:59:59"


def test_delete() -> None:
    freeze_id = "550e8400-e29b-41d4-a716-446655440000"

    with respx.mock(base_url="https://api.mergify.com") as mock:
        mock.delete(
            f"/v1/repos/owner/repo/scheduled_freeze/{freeze_id}",
        ).mock(
            return_value=Response(204),
        )

        runner = CliRunner()
        result = runner.invoke(
            freeze,
            [*BASE_ARGS, "delete", freeze_id],
        )
        assert result.exit_code == 0, result.output
        assert "Freeze deleted successfully" in result.output


def test_delete_with_reason() -> None:
    freeze_id = "550e8400-e29b-41d4-a716-446655440000"

    with respx.mock(base_url="https://api.mergify.com") as mock:
        mock.delete(
            f"/v1/repos/owner/repo/scheduled_freeze/{freeze_id}",
        ).mock(
            return_value=Response(204),
        )

        runner = CliRunner()
        result = runner.invoke(
            freeze,
            [
                *BASE_ARGS,
                "delete",
                freeze_id,
                "--reason",
                "Emergency rollback completed",
            ],
        )
        assert result.exit_code == 0, result.output

        request = mock.calls.last.request
        body = json.loads(request.content)
        assert body["delete_reason"] == "Emergency rollback completed"


def test_delete_without_reason_sends_no_body() -> None:
    freeze_id = "550e8400-e29b-41d4-a716-446655440000"

    with respx.mock(base_url="https://api.mergify.com") as mock:
        mock.delete(
            f"/v1/repos/owner/repo/scheduled_freeze/{freeze_id}",
        ).mock(
            return_value=Response(204),
        )

        runner = CliRunner()
        result = runner.invoke(
            freeze,
            [*BASE_ARGS, "delete", freeze_id],
        )
        assert result.exit_code == 0, result.output

        request = mock.calls.last.request
        assert request.content == b""


def test_list_api_error() -> None:
    with respx.mock(base_url="https://api.mergify.com") as mock:
        mock.get("/v1/repos/owner/repo/scheduled_freeze").mock(
            return_value=Response(403, json={"message": "Forbidden"}),
        )

        runner = CliRunner()
        result = runner.invoke(freeze, [*BASE_ARGS, "list"])
        assert result.exit_code != 0


def test_create_api_error() -> None:
    with respx.mock(base_url="https://api.mergify.com") as mock:
        mock.post("/v1/repos/owner/repo/scheduled_freeze").mock(
            return_value=Response(
                422,
                json={"message": "end must be after start"},
            ),
        )

        runner = CliRunner()
        result = runner.invoke(
            freeze,
            [
                *BASE_ARGS,
                "create",
                "--reason",
                "test",
                "--timezone",
                "UTC",
                "-c",
                "base=main",
            ],
        )
        assert result.exit_code != 0


def test_repository_from_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("GITHUB_REPOSITORY", "env-owner/env-repo")

    import asyncio

    from mergify_cli.freeze.cli import _get_default_repository

    result = asyncio.run(_get_default_repository())
    assert result == "env-owner/env-repo"


class TestNaiveDateTimeType:
    def test_valid_datetime(self) -> None:
        from mergify_cli.freeze.cli import NAIVE_DATETIME

        result = NAIVE_DATETIME.convert("2024-06-19T08:00:00", None, None)
        assert result.year == 2024
        assert result.month == 6
        assert result.day == 19
        assert result.hour == 8

    def test_invalid_datetime(self) -> None:
        from mergify_cli.freeze.cli import NAIVE_DATETIME

        with pytest.raises(click.BadParameter, match="Invalid datetime format"):
            NAIVE_DATETIME.convert("not-a-date", None, None)

    def test_passthrough_datetime(self) -> None:
        from mergify_cli.freeze.cli import NAIVE_DATETIME

        dt = datetime.datetime(2024, 6, 19, 8, 0, 0, tzinfo=datetime.UTC)
        result = NAIVE_DATETIME.convert(dt, None, None)
        assert result is dt
