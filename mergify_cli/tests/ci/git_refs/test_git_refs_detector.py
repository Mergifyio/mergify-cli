from __future__ import annotations

import json
from typing import TYPE_CHECKING

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
            "title": "merge-queue: Merge group",
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


def test_yaml_docs_from_fenced_blocks_valid() -> None:
    body = """Some text
```yaml
---
checking_base_sha: xyz789
pull_requests: [{"number": 1}]
previous_failed_batches: []
...
```
More text"""

    result = detector._yaml_docs_from_fenced_blocks(body)

    assert result == detector.MergeQueueMetadata(
        {
            "checking_base_sha": "xyz789",
            "pull_requests": [{"number": 1}],
            "previous_failed_batches": [],
        },
    )


def test_yaml_docs_from_fenced_blocks_no_yaml() -> None:
    body = "No yaml here"

    result = detector._yaml_docs_from_fenced_blocks(body)

    assert result is None


def test_yaml_docs_from_fenced_blocks_empty_yaml() -> None:
    body = """Some text
```yaml
```
More text"""

    result = detector._yaml_docs_from_fenced_blocks(body)

    assert result is None


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
