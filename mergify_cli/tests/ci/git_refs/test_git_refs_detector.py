from __future__ import annotations

import json
from typing import TYPE_CHECKING
from unittest import mock

import pytest

from mergify_cli.ci.git_refs import detector


if TYPE_CHECKING:
    import pathlib


@pytest.mark.parametrize("event_name", ["pull_request", "pull_request_review", "push"])
def test_detect_base_from_repository_default_branch(
    event_name: str,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: pathlib.Path,
) -> None:
    event_data = {"repository": {"default_branch": "main"}}
    event_file = tmp_path / "event.json"
    event_file.write_text(json.dumps(event_data))

    monkeypatch.setenv("GITHUB_EVENT_NAME", event_name)
    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event_file))

    result = detector.detect()

    expected_base_source: detector.ReferencesSource = (
        "github_event_push" if event_name == "push" else "github_event_pull_request"
    )
    assert result == detector.References("main", "HEAD", expected_base_source)


def test_maybe_write_github_outputs(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    event_data = {"before": "abc123", "after": "xyz987"}
    event_file = tmp_path / "event.json"
    event_file.write_text(json.dumps(event_data))

    output_file = tmp_path / "github_output"

    monkeypatch.setenv("GITHUB_OUTPUT", str(output_file))
    monkeypatch.setenv("GITHUB_EVENT_NAME", "push")
    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event_file))

    result = detector.detect()

    result.maybe_write_to_github_outputs()

    content = output_file.read_text()
    expected = """base=abc123
head=xyz987
"""
    assert content == expected


def test_detect_base_from_push_event(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: pathlib.Path,
) -> None:
    event_data = {"before": "abc123", "after": "xyz987"}
    event_file = tmp_path / "event.json"
    event_file.write_text(json.dumps(event_data))

    monkeypatch.setenv("GITHUB_EVENT_NAME", "push")
    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event_file))

    result = detector.detect()

    assert result == detector.References(
        "abc123",
        "xyz987",
        "github_event_push",
    )


def test_detect_base_from_pull_request_event_path(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: pathlib.Path,
) -> None:
    event_data = {
        "pull_request": {
            "number": 1,
            "base": {"sha": "abc123"},
            "head": {"sha": "xyz987"},
        },
    }
    event_file = tmp_path / "event.json"
    event_file.write_text(json.dumps(event_data))

    monkeypatch.setenv("GITHUB_EVENT_NAME", "pull_request")
    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event_file))

    result = detector.detect()

    assert result == detector.References(
        "abc123",
        "xyz987",
        "github_event_pull_request",
    )


def test_detect_base_merge_queue_override(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: pathlib.Path,
) -> None:
    event_data = {
        "pull_request": {
            "number": 1,
            "title": "merge queue: embarking #1 together",
            "body": "```yaml\nchecking_base_sha: xyz789\n```",
            "base": {"sha": "abc123"},
        },
    }
    event_file = tmp_path / "event.json"
    event_file.write_text(json.dumps(event_data))

    monkeypatch.setenv("GITHUB_EVENT_NAME", "pull_request")
    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event_file))

    result = detector.detect()

    assert result == detector.References("xyz789", "HEAD", "merge_queue")


def test_detect_base_no_info(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    event_data: dict[str, str] = {}
    event_file = tmp_path / "event.json"
    event_file.write_text(json.dumps(event_data))

    monkeypatch.setenv("GITHUB_EVENT_NAME", "pull_request")
    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event_file))

    with pytest.raises(
        detector.BaseNotFoundError,
        match="Could not detect base SHA",
    ):
        detector.detect()


def test_detect_no_github_event(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("GITHUB_EVENT_NAME", raising=False)
    monkeypatch.delenv("GITHUB_EVENT_PATH", raising=False)

    result = detector.detect()

    assert result == detector.References("HEAD^", "HEAD", "fallback_last_commit")


def test_detect_push_event_no_info(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    event_data: dict[str, str] = {}
    event_file = tmp_path / "event.json"
    event_file.write_text(json.dumps(event_data))

    monkeypatch.setenv("GITHUB_EVENT_NAME", "push")
    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event_file))

    with pytest.raises(
        detector.BaseNotFoundError,
        match="Could not detect base SHA",
    ):
        detector.detect()


def test_detect_unhandled_event(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: pathlib.Path,
) -> None:
    event_data: dict[str, str] = {}
    event_file = tmp_path / "event.json"
    event_file.write_text(json.dumps(event_data))

    monkeypatch.setenv("GITHUB_EVENT_NAME", "workflow_run")
    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event_file))

    result = detector.detect()

    assert result == detector.References(None, "HEAD", "github_event_other")


def test_detect_buildkite_pull_request(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("GITHUB_EVENT_NAME", raising=False)
    monkeypatch.delenv("GITHUB_EVENT_PATH", raising=False)
    monkeypatch.setenv("BUILDKITE", "true")
    monkeypatch.setenv("BUILDKITE_PULL_REQUEST", "42")
    monkeypatch.setenv("BUILDKITE_PULL_REQUEST_BASE_BRANCH", "main")
    monkeypatch.setenv("BUILDKITE_COMMIT", "abc123")

    result = detector.detect()

    assert result == detector.References(
        "main",
        "abc123",
        "buildkite_pull_request",
    )


def test_detect_buildkite_not_pr_falls_back(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("GITHUB_EVENT_NAME", raising=False)
    monkeypatch.delenv("GITHUB_EVENT_PATH", raising=False)
    monkeypatch.setenv("BUILDKITE", "true")
    monkeypatch.setenv("BUILDKITE_PULL_REQUEST", "false")

    result = detector.detect()

    assert result == detector.References("HEAD^", "HEAD", "fallback_last_commit")


def test_detect_buildkite_merge_queue_from_note(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """When a git note is present, Buildkite MQ builds report checking_base_sha."""
    monkeypatch.delenv("GITHUB_EVENT_NAME", raising=False)
    monkeypatch.delenv("GITHUB_EVENT_PATH", raising=False)
    monkeypatch.setenv("BUILDKITE", "true")
    monkeypatch.setenv("BUILDKITE_PULL_REQUEST", "99")
    monkeypatch.setenv("BUILDKITE_BRANCH", "mergify/merge-queue/abc")
    monkeypatch.setenv("BUILDKITE_COMMIT", "headsha")
    # Base branch env var is also present; the note must take precedence.
    monkeypatch.setenv("BUILDKITE_PULL_REQUEST_BASE_BRANCH", "main")

    with mock.patch(
        "mergify_cli.ci.queue.notes.read_mq_info_note",
        return_value={
            "checking_base_sha": "basesha",
            "pull_requests": [{"number": 1}],
            "previous_failed_batches": [],
        },
    ) as note_mock:
        result = detector.detect()

    note_mock.assert_called_once_with("mergify/merge-queue/abc", "headsha")
    assert result == detector.References("basesha", "headsha", "merge_queue")


def test_detect_buildkite_pr_falls_back_when_no_note(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Missing note means we fall back to the existing base-branch behavior."""
    monkeypatch.delenv("GITHUB_EVENT_NAME", raising=False)
    monkeypatch.delenv("GITHUB_EVENT_PATH", raising=False)
    monkeypatch.setenv("BUILDKITE", "true")
    monkeypatch.setenv("BUILDKITE_PULL_REQUEST", "42")
    monkeypatch.setenv("BUILDKITE_BRANCH", "feature-branch")
    monkeypatch.setenv("BUILDKITE_COMMIT", "abc123")
    monkeypatch.setenv("BUILDKITE_PULL_REQUEST_BASE_BRANCH", "main")

    with mock.patch(
        "mergify_cli.ci.queue.notes.read_mq_info_note",
        return_value=None,
    ):
        result = detector.detect()

    assert result == detector.References("main", "abc123", "buildkite_pull_request")


def test_detect_gha_merge_queue_from_note(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: pathlib.Path,
) -> None:
    """GitHub Actions PR event: the note takes precedence over PR body parsing."""
    event_data = {
        "pull_request": {
            "number": 7,
            "title": "regular PR title",
            "head": {"ref": "mergify/merge-queue/abc", "sha": "headsha"},
            "base": {"sha": "pr-base-sha"},
        },
    }
    event_file = tmp_path / "event.json"
    event_file.write_text(json.dumps(event_data))

    monkeypatch.setenv("GITHUB_EVENT_NAME", "pull_request")
    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event_file))

    with mock.patch(
        "mergify_cli.ci.queue.notes.read_mq_info_note",
        return_value={
            "checking_base_sha": "mq-base-sha",
            "pull_requests": [{"number": 7}],
            "previous_failed_batches": [],
        },
    ) as note_mock:
        result = detector.detect()

    note_mock.assert_called_once_with("mergify/merge-queue/abc", "headsha")
    assert result == detector.References("mq-base-sha", "headsha", "merge_queue")


def test_detect_gha_falls_back_to_pr_base_when_no_note(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: pathlib.Path,
) -> None:
    """Absent note → current PR-base-SHA path is preserved."""
    event_data = {
        "pull_request": {
            "number": 7,
            "title": "regular PR title",
            "head": {"ref": "feature-branch", "sha": "headsha"},
            "base": {"sha": "pr-base-sha"},
        },
    }
    event_file = tmp_path / "event.json"
    event_file.write_text(json.dumps(event_data))

    monkeypatch.setenv("GITHUB_EVENT_NAME", "pull_request")
    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event_file))

    with mock.patch(
        "mergify_cli.ci.queue.notes.read_mq_info_note",
        return_value=None,
    ):
        result = detector.detect()

    assert result == detector.References(
        "pr-base-sha",
        "headsha",
        "github_event_pull_request",
    )
