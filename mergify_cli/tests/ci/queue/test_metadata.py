from __future__ import annotations

import json
from typing import TYPE_CHECKING

from mergify_cli.ci.queue import metadata


if TYPE_CHECKING:
    import pathlib

    import pytest


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

    result = metadata._yaml_docs_from_fenced_blocks(body)

    assert result == {
        "checking_base_sha": "xyz789",
        "pull_requests": [{"number": 1}],
        "previous_failed_batches": [],
    }


def test_yaml_docs_from_fenced_blocks_no_yaml() -> None:
    body = "No yaml here"

    result = metadata._yaml_docs_from_fenced_blocks(body)

    assert result is None


def test_yaml_docs_from_fenced_blocks_empty_yaml() -> None:
    body = """Some text
```yaml
```
More text"""

    result = metadata._yaml_docs_from_fenced_blocks(body)

    assert result is None


def test_detect_merge_queue(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: pathlib.Path,
) -> None:
    event_data = {
        "pull_request": {
            "number": 10,
            "title": "merge queue: embarking #1 and #2 together",
            "body": "```yaml\n---\nchecking_base_sha: xyz789\npull_requests:\n  - number: 1\n  - number: 2\nprevious_failed_batches:\n  - draft_pr_number: 5\n    checked_pull_requests:\n      - 1\n      - 3\n...\n```",
            "base": {"sha": "abc123"},
        },
    }
    event_file = tmp_path / "event.json"
    event_file.write_text(json.dumps(event_data))

    monkeypatch.setenv("GITHUB_EVENT_NAME", "pull_request")
    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event_file))

    result = metadata.detect()

    assert result is not None
    assert result["checking_base_sha"] == "xyz789"
    assert result["pull_requests"] == [{"number": 1}, {"number": 2}]
    assert result["previous_failed_batches"] == [
        {"draft_pr_number": 5, "checked_pull_requests": [1, 3]},
    ]


def test_detect_merge_queue_no_body(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: pathlib.Path,
) -> None:
    event_data = {
        "pull_request": {
            "number": 10,
            "title": "merge queue: embarking #1 together",
        },
    }
    event_file = tmp_path / "event.json"
    event_file.write_text(json.dumps(event_data))

    monkeypatch.setenv("GITHUB_EVENT_NAME", "pull_request")
    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event_file))

    assert metadata.detect() is None


def test_detect_merge_queue_body_without_yaml(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: pathlib.Path,
) -> None:
    event_data = {
        "pull_request": {
            "number": 10,
            "title": "merge queue: embarking #1 together",
            "body": "No yaml metadata here",
        },
    }
    event_file = tmp_path / "event.json"
    event_file.write_text(json.dumps(event_data))

    monkeypatch.setenv("GITHUB_EVENT_NAME", "pull_request")
    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event_file))

    assert metadata.detect() is None


def test_detect_null_title(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: pathlib.Path,
) -> None:
    event_data = {
        "pull_request": {
            "number": 10,
            "title": None,
        },
    }
    event_file = tmp_path / "event.json"
    event_file.write_text(json.dumps(event_data))

    monkeypatch.setenv("GITHUB_EVENT_NAME", "pull_request")
    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event_file))

    assert metadata.detect() is None


def test_detect_not_merge_queue(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: pathlib.Path,
) -> None:
    event_data = {
        "pull_request": {
            "number": 5,
            "title": "feat: add something",
            "body": "Some description",
            "base": {"sha": "abc123"},
        },
    }
    event_file = tmp_path / "event.json"
    event_file.write_text(json.dumps(event_data))

    monkeypatch.setenv("GITHUB_EVENT_NAME", "pull_request")
    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event_file))

    assert metadata.detect() is None


def test_detect_no_event(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("GITHUB_EVENT_NAME", raising=False)
    monkeypatch.delenv("GITHUB_EVENT_PATH", raising=False)

    assert metadata.detect() is None


def test_detect_push_event(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: pathlib.Path,
) -> None:
    event_data = {"before": "abc123", "after": "xyz987"}
    event_file = tmp_path / "event.json"
    event_file.write_text(json.dumps(event_data))

    monkeypatch.setenv("GITHUB_EVENT_NAME", "push")
    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event_file))

    assert metadata.detect() is None
