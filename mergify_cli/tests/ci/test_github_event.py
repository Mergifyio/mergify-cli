from __future__ import annotations

import json
import pathlib

import pydantic
import pytest

from mergify_cli.ci.github_event import GitHubEvent


PULL_REQUEST_EVENT = pathlib.Path(__file__).parent / "pull_request.json"
PUSH_EVENT = pathlib.Path(__file__).parent / "push_event.json"


def test_parse_real_pull_request_event() -> None:
    raw = json.loads(PULL_REQUEST_EVENT.read_bytes())
    event = GitHubEvent.model_validate(raw)

    assert event.pull_request is not None
    assert event.pull_request.number == 2
    assert event.pull_request.title == "Update the README with new information."
    assert event.pull_request.body is not None

    assert event.pull_request.head is not None
    assert event.pull_request.head.sha == "ec26c3e57ca3a959ca5aad62de7213c562f8c821"
    assert event.pull_request.head.ref == "changes"

    assert event.pull_request.base is not None
    assert event.pull_request.base.sha == "f95f852bd8fca8fcc58a9a2d6c842781e32a215e"
    assert event.pull_request.base.ref == "master"

    assert event.repository is not None
    assert event.repository.default_branch == "master"

    # push-event fields should be None
    assert event.before is None
    assert event.after is None


def test_parse_real_push_event() -> None:
    raw = json.loads(PUSH_EVENT.read_bytes())
    event = GitHubEvent.model_validate(raw)

    assert event.before == "773db6b5c5f77d0c70c75e6dacef1684cb03495f"
    assert event.after == "10068d193546082d802676bb310a570d0898e061"
    assert event.pull_request is None
    assert event.repository is not None
    assert event.repository.default_branch == "main"


def test_parse_empty_event() -> None:
    event = GitHubEvent.model_validate({})

    assert event.pull_request is None
    assert event.repository is None
    assert event.before is None
    assert event.after is None


def test_parse_minimal_pull_request() -> None:
    raw = {"pull_request": {"number": 42}}
    event = GitHubEvent.model_validate(raw)

    assert event.pull_request is not None
    assert event.pull_request.number == 42
    assert not event.pull_request.title
    assert event.pull_request.body is None
    assert event.pull_request.base is None
    assert event.pull_request.head is None


def test_parse_pull_request_missing_number() -> None:
    with pytest.raises(pydantic.ValidationError, match="number"):
        GitHubEvent.model_validate({"pull_request": {"title": "oops"}})
