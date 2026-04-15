from __future__ import annotations

import datetime
import typing
from unittest.mock import patch

from click.testing import CliRunner
from httpx import Response
import respx

from mergify_cli.exit_codes import ExitCode
from mergify_cli.queue.cli import _relative_time
from mergify_cli.queue.cli import _topological_sort
from mergify_cli.queue.cli import queue


FAKE_PR = {
    "number": 123,
    "title": "Add feature X",
    "url": "https://github.com/owner/repo/pull/123",
    "author": {"id": 1, "login": "octocat"},
    "queued_at": "2025-11-05T10:00:00Z",
    "priority_alias": "medium",
    "priority_rule_name": "default",
    "labels": [],
    "scopes": ["main"],
    "estimated_merge_at": "2025-11-05T11:00:00Z",
}

FAKE_BATCH = {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "name": "batch-1",
    "status": {"code": "running"},
    "started_at": "2025-11-05T10:00:00Z",
    "estimated_merge_at": "2025-11-05T11:00:00Z",
    "checks_summary": {"passed": 5, "total": 10},
    "pull_requests": [FAKE_PR],
    "parent_ids": [],
    "scopes": ["main"],
    "sub_batches": None,
}

FAKE_PAUSE = {
    "reason": "Deploying hotfix",
    "paused_at": "2025-11-05T14:00:00Z",
}

BASE_ARGS = [
    "--token",
    "test-token",
    "--api-url",
    "https://api.mergify.com",
    "--repository",
    "owner/repo",
]


def _invoke_status(
    mock: respx.MockRouter,
    response_json: dict[str, typing.Any],
    extra_args: list[str] | None = None,
) -> typing.Any:
    mock.get("/v1/repos/owner/repo/merge-queue/status").mock(
        return_value=Response(200, json=response_json),
    )
    runner = CliRunner()
    args = [*BASE_ARGS, "status", *(extra_args or [])]
    return runner.invoke(queue, args)


class TestRelativeTime:
    def test_seconds(self) -> None:
        now = datetime.datetime(2025, 1, 1, 12, 0, 30, tzinfo=datetime.UTC)
        with patch("mergify_cli.queue.cli.datetime") as mock_dt:
            mock_dt.datetime.now.return_value = now
            mock_dt.datetime.fromisoformat = datetime.datetime.fromisoformat
            mock_dt.UTC = datetime.UTC
            assert _relative_time("2025-01-01T12:00:00Z") == "30s ago"

    def test_minutes(self) -> None:
        now = datetime.datetime(2025, 1, 1, 12, 5, 0, tzinfo=datetime.UTC)
        with patch("mergify_cli.queue.cli.datetime") as mock_dt:
            mock_dt.datetime.now.return_value = now
            mock_dt.datetime.fromisoformat = datetime.datetime.fromisoformat
            mock_dt.UTC = datetime.UTC
            assert _relative_time("2025-01-01T12:00:00Z") == "5m ago"

    def test_hours(self) -> None:
        now = datetime.datetime(2025, 1, 1, 14, 0, 0, tzinfo=datetime.UTC)
        with patch("mergify_cli.queue.cli.datetime") as mock_dt:
            mock_dt.datetime.now.return_value = now
            mock_dt.datetime.fromisoformat = datetime.datetime.fromisoformat
            mock_dt.UTC = datetime.UTC
            assert _relative_time("2025-01-01T12:00:00Z") == "2h ago"

    def test_days(self) -> None:
        now = datetime.datetime(2025, 1, 4, 12, 0, 0, tzinfo=datetime.UTC)
        with patch("mergify_cli.queue.cli.datetime") as mock_dt:
            mock_dt.datetime.now.return_value = now
            mock_dt.datetime.fromisoformat = datetime.datetime.fromisoformat
            mock_dt.UTC = datetime.UTC
            assert _relative_time("2025-01-01T12:00:00Z") == "3d ago"

    def test_future(self) -> None:
        now = datetime.datetime(2025, 1, 1, 12, 0, 0, tzinfo=datetime.UTC)
        with patch("mergify_cli.queue.cli.datetime") as mock_dt:
            mock_dt.datetime.now.return_value = now
            mock_dt.datetime.fromisoformat = datetime.datetime.fromisoformat
            mock_dt.UTC = datetime.UTC
            assert _relative_time("2025-01-01T12:30:00Z", future=True) == "~30m"

    def test_none(self) -> None:
        assert not _relative_time(None)

    def test_empty(self) -> None:
        assert not _relative_time("")


class TestTopologicalSort:
    def test_no_parents(self) -> None:
        batches = [
            {**FAKE_BATCH, "id": "a", "parent_ids": []},
            {**FAKE_BATCH, "id": "b", "parent_ids": []},
        ]
        result = _topological_sort(batches)  # type: ignore[arg-type]
        assert [b["id"] for b in result] == ["a", "b"]

    def test_chain(self) -> None:
        batches = [
            {**FAKE_BATCH, "id": "c", "parent_ids": ["b"]},
            {**FAKE_BATCH, "id": "a", "parent_ids": []},
            {**FAKE_BATCH, "id": "b", "parent_ids": ["a"]},
        ]
        result = _topological_sort(batches)  # type: ignore[arg-type]
        assert [b["id"] for b in result] == ["a", "b", "c"]

    def test_diamond(self) -> None:
        batches = [
            {**FAKE_BATCH, "id": "d", "parent_ids": ["b", "c"]},
            {**FAKE_BATCH, "id": "b", "parent_ids": ["a"]},
            {**FAKE_BATCH, "id": "c", "parent_ids": ["a"]},
            {**FAKE_BATCH, "id": "a", "parent_ids": []},
        ]
        result = _topological_sort(batches)  # type: ignore[arg-type]
        ids = [b["id"] for b in result]
        assert ids.index("a") < ids.index("b")
        assert ids.index("a") < ids.index("c")
        assert ids.index("b") < ids.index("d")
        assert ids.index("c") < ids.index("d")


class TestStatusCommand:
    def test_empty_queue(self) -> None:
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_status(
                mock,
                {
                    "batches": [],
                    "waiting_pull_requests": [],
                    "scope_queues": {},
                },
            )
        assert result.exit_code == 0, result.output
        assert "Merge Queue: owner/repo" in result.output
        assert "Queue is empty" in result.output

    def test_with_batches(self) -> None:
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_status(
                mock,
                {
                    "batches": [FAKE_BATCH],
                    "waiting_pull_requests": [],
                    "scope_queues": {},
                },
            )
        assert result.exit_code == 0, result.output
        assert "Batches" in result.output
        assert "running" in result.output
        assert "5/10" in result.output
        assert "#123" in result.output
        assert "Add feature X" in result.output
        assert "octocat" in result.output

    def test_with_waiting_prs(self) -> None:
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_status(
                mock,
                {
                    "batches": [],
                    "waiting_pull_requests": [FAKE_PR],
                    "scope_queues": {},
                },
            )
        assert result.exit_code == 0, result.output
        assert "Waiting" in result.output
        assert "#123" in result.output
        assert "Add feature X" in result.output
        assert "octocat" in result.output
        assert "medium" in result.output

    def test_with_batches_and_waiting_prs(self) -> None:
        waiting_pr = {
            **FAKE_PR,
            "number": 456,
            "title": "Another PR",
            "author": {"id": 2, "login": "hubot"},
        }
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_status(
                mock,
                {
                    "batches": [FAKE_BATCH],
                    "waiting_pull_requests": [waiting_pr],
                    "scope_queues": {},
                },
            )
        assert result.exit_code == 0, result.output
        assert "Batches" in result.output
        assert "Waiting" in result.output
        assert "#123" in result.output
        assert "#456" in result.output

    def test_paused(self) -> None:
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_status(
                mock,
                {
                    "batches": [FAKE_BATCH],
                    "waiting_pull_requests": [],
                    "scope_queues": {},
                    "pause": FAKE_PAUSE,
                },
            )
        assert result.exit_code == 0, result.output
        assert "paused" in result.output.lower()
        assert "Deploying hotfix" in result.output

    def test_paused_empty_queue(self) -> None:
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_status(
                mock,
                {
                    "batches": [],
                    "waiting_pull_requests": [],
                    "scope_queues": {},
                    "pause": FAKE_PAUSE,
                },
            )
        assert result.exit_code == 0, result.output
        assert "paused" in result.output.lower()
        assert "Queue is empty" in result.output

    def test_json_output(self) -> None:
        import json

        api_response = {
            "batches": [FAKE_BATCH],
            "waiting_pull_requests": [FAKE_PR],
            "scope_queues": {},
        }
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_status(mock, api_response, extra_args=["--json"])
        assert result.exit_code == 0, result.output
        data = json.loads(result.output)
        assert len(data["batches"]) == 1
        assert len(data["waiting_pull_requests"]) == 1

    def test_branch_filter(self) -> None:
        with respx.mock(base_url="https://api.mergify.com") as mock:
            route = mock.get(
                "/v1/repos/owner/repo/merge-queue/status",
                params={"branch": "release"},
            ).mock(
                return_value=Response(
                    200,
                    json={
                        "batches": [],
                        "waiting_pull_requests": [],
                        "scope_queues": {},
                    },
                ),
            )
            runner = CliRunner()
            result = runner.invoke(
                queue,
                [*BASE_ARGS, "status", "--branch", "release"],
            )
        assert result.exit_code == 0, result.output
        assert route.called

    def test_api_error(self) -> None:
        with respx.mock(base_url="https://api.mergify.com") as mock:
            mock.get("/v1/repos/owner/repo/merge-queue/status").mock(
                return_value=Response(403, json={"message": "Forbidden"}),
            )
            runner = CliRunner()
            result = runner.invoke(queue, [*BASE_ARGS, "status"])
        assert result.exit_code != 0

    def test_pr_without_eta(self) -> None:
        pr_no_eta = {**FAKE_PR, "estimated_merge_at": None}
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_status(
                mock,
                {
                    "batches": [],
                    "waiting_pull_requests": [pr_no_eta],
                    "scope_queues": {},
                },
            )
        assert result.exit_code == 0, result.output
        assert "#123" in result.output

    def test_multi_scope(self) -> None:
        batch_main = {
            **FAKE_BATCH,
            "id": "aaa",
            "scopes": ["main"],
        }
        batch_staging = {
            **FAKE_BATCH,
            "id": "bbb",
            "scopes": ["staging"],
            "status": {"code": "preparing"},
            "pull_requests": [
                {
                    **FAKE_PR,
                    "number": 456,
                    "title": "Staging fix",
                    "author": {"id": 2, "login": "hubot"},
                },
            ],
        }
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_status(
                mock,
                {
                    "batches": [batch_main, batch_staging],
                    "waiting_pull_requests": [],
                    "scope_queues": {},
                },
            )
        assert result.exit_code == 0, result.output
        assert "main" in result.output
        assert "staging" in result.output
        assert "#123" in result.output
        assert "#456" in result.output

    def test_multi_pr_batch(self) -> None:
        pr2 = {
            **FAKE_PR,
            "number": 789,
            "title": "Second PR",
            "author": {"id": 3, "login": "alice"},
        }
        batch = {
            **FAKE_BATCH,
            "pull_requests": [FAKE_PR, pr2],
        }
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_status(
                mock,
                {
                    "batches": [batch],
                    "waiting_pull_requests": [],
                    "scope_queues": {},
                },
            )
        assert result.exit_code == 0, result.output
        assert "#123" in result.output
        assert "#789" in result.output
        assert "alice" in result.output

    def test_status_icons(self) -> None:
        batch_failed = {
            **FAKE_BATCH,
            "status": {"code": "failed"},
        }
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_status(
                mock,
                {
                    "batches": [batch_failed],
                    "waiting_pull_requests": [],
                    "scope_queues": {},
                },
            )
        assert result.exit_code == 0, result.output
        assert "failed" in result.output

    def test_checks_omitted_when_zero(self) -> None:
        batch_no_checks = {
            **FAKE_BATCH,
            "checks_summary": {"passed": 0, "total": 0},
        }
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_status(
                mock,
                {
                    "batches": [batch_no_checks],
                    "waiting_pull_requests": [],
                    "scope_queues": {},
                },
            )
        assert result.exit_code == 0, result.output
        assert "0/0" not in result.output


FAKE_PAUSE_RESPONSE = {
    "paused": True,
    "reason": "Deploying hotfix",
    "paused_at": "2025-11-05T14:00:00Z",
}


class TestPauseCommand:
    def test_pause_with_confirmation(self) -> None:
        with respx.mock(base_url="https://api.mergify.com") as mock:
            mock.put("/v1/repos/owner/repo/merge-queue/pause").mock(
                return_value=Response(200, json=FAKE_PAUSE_RESPONSE),
            )
            runner = CliRunner()
            with patch("os.isatty", return_value=True):
                result = runner.invoke(
                    queue,
                    [*BASE_ARGS, "pause", "--reason", "Deploying hotfix"],
                    input="y\n",
                )
        assert result.exit_code == 0, result.output
        assert "paused" in result.output.lower()
        assert "Deploying hotfix" in result.output

    def test_pause_with_yes_flag(self) -> None:
        with respx.mock(base_url="https://api.mergify.com") as mock:
            mock.put("/v1/repos/owner/repo/merge-queue/pause").mock(
                return_value=Response(200, json=FAKE_PAUSE_RESPONSE),
            )
            runner = CliRunner()
            result = runner.invoke(
                queue,
                [
                    *BASE_ARGS,
                    "pause",
                    "--reason",
                    "Deploying hotfix",
                    "--yes-i-am-sure",
                ],
            )
        assert result.exit_code == 0, result.output
        assert "paused" in result.output.lower()
        assert "Deploying hotfix" in result.output

    def test_pause_confirmation_denied(self) -> None:
        runner = CliRunner()
        with patch("os.isatty", return_value=True):
            result = runner.invoke(
                queue,
                [*BASE_ARGS, "pause", "--reason", "test"],
                input="n\n",
            )
        assert result.exit_code != 0

    def test_pause_non_tty_without_flag(self) -> None:
        runner = CliRunner()
        with patch("os.isatty", return_value=False):
            result = runner.invoke(
                queue,
                [*BASE_ARGS, "pause", "--reason", "test"],
            )
        assert result.exit_code == ExitCode.INVALID_STATE
        assert "--yes-i-am-sure" in result.output

    def test_pause_requires_reason(self) -> None:
        runner = CliRunner()
        result = runner.invoke(
            queue,
            [*BASE_ARGS, "pause"],
        )
        assert result.exit_code != 0

    def test_pause_reason_too_long(self) -> None:
        runner = CliRunner()
        result = runner.invoke(
            queue,
            [*BASE_ARGS, "pause", "--reason", "x" * 256, "--yes-i-am-sure"],
        )
        assert result.exit_code != 0
        assert "255 characters" in result.output

    def test_pause_api_error(self) -> None:
        with respx.mock(base_url="https://api.mergify.com") as mock:
            mock.put("/v1/repos/owner/repo/merge-queue/pause").mock(
                return_value=Response(422, json={"message": "Invalid reason"}),
            )
            runner = CliRunner()
            result = runner.invoke(
                queue,
                [
                    *BASE_ARGS,
                    "pause",
                    "--reason",
                    "test",
                    "--yes-i-am-sure",
                ],
            )
        assert result.exit_code != 0


class TestUnpauseCommand:
    def test_unpause(self) -> None:
        with respx.mock(base_url="https://api.mergify.com") as mock:
            mock.delete("/v1/repos/owner/repo/merge-queue/pause").mock(
                return_value=Response(204),
            )
            runner = CliRunner()
            result = runner.invoke(queue, [*BASE_ARGS, "unpause"])
        assert result.exit_code == 0, result.output
        assert "unpaused" in result.output.lower()

    def test_unpause_not_paused(self) -> None:
        with respx.mock(base_url="https://api.mergify.com") as mock:
            mock.delete("/v1/repos/owner/repo/merge-queue/pause").mock(
                return_value=Response(404, json={"message": "Not paused"}),
            )
            runner = CliRunner()
            result = runner.invoke(queue, [*BASE_ARGS, "unpause"])
        assert result.exit_code == ExitCode.MERGIFY_API_ERROR
        assert "not currently paused" in result.output.lower()
