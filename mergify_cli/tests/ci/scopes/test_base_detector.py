import json
import pathlib

import pytest

from mergify_cli.ci.scopes import base_detector


def test_detect_base_github_base_ref(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("GITHUB_BASE_REF", "main")
    monkeypatch.delenv("GITHUB_EVENT_PATH", raising=False)

    result = base_detector.detect()

    assert result == base_detector.Base("main", is_merge_queue=False)


def test_detect_base_from_event_path(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: pathlib.Path,
) -> None:
    event_data = {
        "pull_request": {
            "base": {"sha": "abc123"},
        },
    }
    event_file = tmp_path / "event.json"
    event_file.write_text(json.dumps(event_data))

    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event_file))
    monkeypatch.delenv("GITHUB_BASE_REF", raising=False)

    result = base_detector.detect()

    assert result == base_detector.Base("abc123", is_merge_queue=False)


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

    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event_file))

    result = base_detector.detect()

    assert result == base_detector.Base("xyz789", is_merge_queue=True)


def test_detect_base_no_info(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("GITHUB_EVENT_PATH", raising=False)
    monkeypatch.delenv("GITHUB_BASE_REF", raising=False)

    with pytest.raises(
        base_detector.BaseNotFoundError,
        match="Could not detect base SHA",
    ):
        base_detector.detect()


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

    result = base_detector._yaml_docs_from_fenced_blocks(body)

    assert result == base_detector.MergeQueueMetadata(
        {
            "checking_base_sha": "xyz789",
            "pull_requests": [{"number": 1}],
            "previous_failed_batches": [],
        },
    )


def test_yaml_docs_from_fenced_blocks_no_yaml() -> None:
    body = "No yaml here"

    result = base_detector._yaml_docs_from_fenced_blocks(body)

    assert result is None


def test_yaml_docs_from_fenced_blocks_empty_yaml() -> None:
    body = """Some text
```yaml
```
More text"""

    result = base_detector._yaml_docs_from_fenced_blocks(body)

    assert result is None
