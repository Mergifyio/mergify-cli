from __future__ import annotations

import datetime
import json
import typing
from unittest.mock import patch

from click.testing import CliRunner
from httpx import Response
import respx

from mergify_cli.exit_codes import ExitCode
from mergify_cli.queue.cli import queue


FAKE_CHECK_SUCCESS = {
    "name": "tests",
    "description": "Running tests",
    "url": "https://github.com/owner/repo/actions/runs/123",
    "state": "success",
    "avatar_url": None,
}

FAKE_CHECK_PENDING = {
    "name": "linters",
    "description": "Running linters",
    "url": None,
    "state": "pending",
    "avatar_url": None,
}

FAKE_CHECK_FAILED = {
    "name": "security-scan",
    "description": "Security scan",
    "url": None,
    "state": "failure",
    "avatar_url": None,
}

FAKE_CONDITION_MATCH = {
    "match": True,
    "label": "#check-success=tests",
    "description": None,
    "subconditions": [],
    "evaluations": [],
}

FAKE_CONDITION_NO_MATCH = {
    "match": False,
    "label": "#check-success=linters",
    "description": None,
    "subconditions": [],
    "evaluations": [],
}

FAKE_CONDITIONS_EVALUATION = {
    "match": False,
    "label": "all of",
    "description": None,
    "subconditions": [FAKE_CONDITION_MATCH, FAKE_CONDITION_NO_MATCH],
    "evaluations": [],
}

FAKE_MERGEABILITY_CHECK = {
    "check_type": "in_place",
    "queue_pull_request_number": 123,
    "started_at": "2025-11-05T10:05:00Z",
    "ci_ended_at": None,
    "ci_state": "pending",
    "state": "running",
    "checks": [FAKE_CHECK_SUCCESS, FAKE_CHECK_PENDING],
    "conditions_evaluation": FAKE_CONDITIONS_EVALUATION,
}

FAKE_PULL_RESPONSE = {
    "number": 123,
    "queued_at": "2025-11-05T10:00:00Z",
    "estimated_time_of_merge": "2025-11-05T11:00:00Z",
    "position": 3,
    "priority_rule_name": "default",
    "queue_rule_name": "default",
    "checks_timeout_at": "2025-11-05T12:00:00Z",
    "queue_rule": {"name": "default", "config": {}},
    "mergeability_check": FAKE_MERGEABILITY_CHECK,
}

BASE_ARGS = [
    "--token",
    "test-token",
    "--api-url",
    "https://api.mergify.com",
    "--repository",
    "owner/repo",
]


def _invoke_show(
    mock: respx.MockRouter,
    pr_number: int,
    response_json: dict[str, typing.Any],
    *,
    status_code: int = 200,
    extra_args: list[str] | None = None,
) -> typing.Any:
    mock.get(f"/v1/repos/owner/repo/merge-queue/pull/{pr_number}").mock(
        return_value=Response(status_code, json=response_json),
    )
    runner = CliRunner()
    args = [*BASE_ARGS, "show", str(pr_number), *(extra_args or [])]
    return runner.invoke(queue, args)


class TestShowCommand:
    def test_compact_output(self) -> None:
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_show(mock, 123, FAKE_PULL_RESPONSE)
        assert result.exit_code == 0, result.output
        assert "PR #123" in result.output
        assert "Position:    3" in result.output
        assert "Priority:    default" in result.output
        assert "Queue rule:  default" in result.output
        assert "pending" in result.output
        assert "passed" in result.output
        assert "met" in result.output

    def test_verbose_output(self) -> None:
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_show(mock, 123, FAKE_PULL_RESPONSE, extra_args=["-v"])
        assert result.exit_code == 0, result.output
        assert "PR #123" in result.output
        assert "tests" in result.output
        assert "linters" in result.output
        assert "Conditions" in result.output

    def test_metadata_fields(self) -> None:
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_show(mock, 123, FAKE_PULL_RESPONSE)
        assert result.exit_code == 0, result.output
        assert "Priority" in result.output
        assert "Queue rule" in result.output
        assert "Queued at" in result.output
        assert "ETA" in result.output

    def test_compact_checks_summary(self) -> None:
        response = {
            **FAKE_PULL_RESPONSE,
            "mergeability_check": {
                **FAKE_MERGEABILITY_CHECK,
                "checks": [FAKE_CHECK_SUCCESS, FAKE_CHECK_PENDING, FAKE_CHECK_FAILED],
            },
        }
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_show(mock, 123, response)
        assert result.exit_code == 0, result.output
        assert "1 passed" in result.output
        assert "1 pending" in result.output
        assert "1 failed" in result.output
        assert "security-scan" in result.output

    def test_verbose_checks_table(self) -> None:
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_show(mock, 123, FAKE_PULL_RESPONSE, extra_args=["-v"])
        assert result.exit_code == 0, result.output
        assert "tests" in result.output
        assert "linters" in result.output

    def test_compact_failing_conditions(self) -> None:
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_show(mock, 123, FAKE_PULL_RESPONSE)
        assert result.exit_code == 0, result.output
        assert "1/2 met" in result.output
        assert "#check-success=linters" in result.output

    def test_verbose_conditions_tree(self) -> None:
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_show(mock, 123, FAKE_PULL_RESPONSE, extra_args=["-v"])
        assert result.exit_code == 0, result.output
        assert "#check-success=tests" in result.output
        assert "#check-success=linters" in result.output

    def test_no_mergeability_check(self) -> None:
        response = {**FAKE_PULL_RESPONSE, "mergeability_check": None}
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_show(mock, 123, response)
        assert result.exit_code == 0, result.output
        assert "Waiting for mergeability check" in result.output

    def test_not_in_queue_404(self) -> None:
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_show(
                mock,
                999,
                {"message": "Not Found"},
                status_code=404,
            )
        assert result.exit_code == ExitCode.MERGIFY_API_ERROR
        assert "not in the merge queue" in result.output

    def test_json_output(self) -> None:
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_show(
                mock,
                123,
                FAKE_PULL_RESPONSE,
                extra_args=["--json"],
            )
        assert result.exit_code == 0, result.output
        data = json.loads(result.output)
        assert data["number"] == 123
        assert data["position"] == 3
        assert data["mergeability_check"]["ci_state"] == "pending"

    def test_no_eta(self) -> None:
        response = {**FAKE_PULL_RESPONSE, "estimated_time_of_merge": None}
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_show(mock, 123, response)
        assert result.exit_code == 0, result.output
        assert "ETA" in result.output

    def test_nested_conditions_verbose(self) -> None:
        nested = {
            "match": False,
            "label": "all of",
            "description": None,
            "subconditions": [
                {
                    "match": True,
                    "label": "any of",
                    "description": None,
                    "subconditions": [
                        {
                            "match": True,
                            "label": "label=ready",
                            "description": None,
                            "subconditions": [],
                            "evaluations": [],
                        },
                    ],
                    "evaluations": [],
                },
                FAKE_CONDITION_NO_MATCH,
            ],
            "evaluations": [],
        }
        response = {
            **FAKE_PULL_RESPONSE,
            "mergeability_check": {
                **FAKE_MERGEABILITY_CHECK,
                "conditions_evaluation": nested,
            },
        }
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_show(mock, 123, response, extra_args=["-v"])
        assert result.exit_code == 0, result.output
        assert "label=ready" in result.output
        assert "#check-success=linters" in result.output

    def test_api_error_non_404(self) -> None:
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_show(
                mock,
                123,
                {"message": "Forbidden"},
                status_code=403,
            )
        assert result.exit_code != 0

    def test_no_conditions_evaluation(self) -> None:
        response = {
            **FAKE_PULL_RESPONSE,
            "mergeability_check": {
                **FAKE_MERGEABILITY_CHECK,
                "conditions_evaluation": None,
            },
        }
        with respx.mock(base_url="https://api.mergify.com") as mock:
            result = _invoke_show(mock, 123, response)
        assert result.exit_code == 0, result.output
        assert "CI State" in result.output
        assert "Conditions" not in result.output


FIXED_NOW = datetime.datetime(2025, 11, 5, 10, 10, 0, tzinfo=datetime.UTC)


class TestShowOutputSnapshot:
    """Full output snapshot tests to visually assess UX from tests."""

    def _invoke_with_fixed_time(
        self,
        response_json: dict[str, typing.Any],
        extra_args: list[str] | None = None,
    ) -> typing.Any:
        with (
            patch("mergify_cli.queue.cli.datetime") as mock_dt,
            respx.mock(base_url="https://api.mergify.com") as mock,
        ):
            mock_dt.datetime.now.return_value = FIXED_NOW
            mock_dt.datetime.fromisoformat = datetime.datetime.fromisoformat
            mock_dt.UTC = datetime.UTC
            mock.get("/v1/repos/owner/repo/merge-queue/pull/123").mock(
                return_value=Response(200, json=response_json),
            )
            runner = CliRunner()
            args = [*BASE_ARGS, "show", "123", *(extra_args or [])]
            return runner.invoke(queue, args)

    def test_compact_snapshot(self) -> None:
        result = self._invoke_with_fixed_time(FAKE_PULL_RESPONSE)
        assert result.exit_code == 0, result.output
        assert result.output == (
            "PR #123\n"
            "\n"
            "  Position:    3\n"
            "  Priority:    default\n"
            "  Queue rule:  default\n"
            "  Queued at:   10m ago\n"
            "  ETA:         ~50m\n"
            "\n"
            "  CI State: ◌ pending   in_place   started 5m ago\n"
            "  Checks:  1 passed, 1 pending\n"
            "\n"
            "  Conditions: 1/2 met\n"
            "  ✗ #check-success=linters\n"
        )

    def test_verbose_snapshot(self) -> None:
        result = self._invoke_with_fixed_time(FAKE_PULL_RESPONSE, extra_args=["-v"])
        assert result.exit_code == 0, result.output
        assert result.output == (
            "PR #123\n"
            "\n"
            "  Position:    3\n"
            "  Priority:    default\n"
            "  Queue rule:  default\n"
            "  Queued at:   10m ago\n"
            "  ETA:         ~50m\n"
            "\n"
            "  CI State: ◌ pending   in_place   started 5m ago\n"
            "   Check    Status    \n"
            "   tests    ✓ success \n"
            "   linters  ◌ pending \n"
            "\n"
            "Conditions\n"
            "├── ✓ #check-success=tests\n"
            "└── ✗ #check-success=linters\n"
        )

    def test_no_mergeability_snapshot(self) -> None:
        response = {**FAKE_PULL_RESPONSE, "mergeability_check": None}
        result = self._invoke_with_fixed_time(response)
        assert result.exit_code == 0, result.output
        assert result.output == (
            "PR #123\n"
            "\n"
            "  Position:    3\n"
            "  Priority:    default\n"
            "  Queue rule:  default\n"
            "  Queued at:   10m ago\n"
            "  ETA:         ~50m\n"
            "\n"
            "  Waiting for mergeability check...\n"
        )
