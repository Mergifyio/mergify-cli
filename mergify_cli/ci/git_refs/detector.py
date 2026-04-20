from __future__ import annotations

import dataclasses
import os
import pathlib
import typing

from mergify_cli import utils
from mergify_cli.ci.queue import metadata as queue_metadata
from mergify_cli.ci.queue import notes as queue_notes
from mergify_cli.ci.scopes import exceptions


if typing.TYPE_CHECKING:
    from mergify_cli.ci import github_event


GITHUB_ACTIONS_BASE_OUTPUT_NAME = "base"
GITHUB_ACTIONS_HEAD_OUTPUT_NAME = "head"


class BaseNotFoundError(exceptions.ScopesError):
    pass


ReferencesSource = typing.Literal[
    "manual",
    "merge_queue",
    "fallback_last_commit",
    "github_event_other",
    "github_event_pull_request",
    "github_event_push",
    "buildkite_pull_request",
]


@dataclasses.dataclass
class References:
    base: str | None
    head: str
    source: ReferencesSource

    def maybe_write_to_github_outputs(self) -> None:
        gha = os.environ.get("GITHUB_OUTPUT")
        if not gha:
            return
        with pathlib.Path(gha).open("a", encoding="utf-8") as fh:
            fh.write(f"{GITHUB_ACTIONS_BASE_OUTPUT_NAME}={self.base}\n")
            fh.write(f"{GITHUB_ACTIONS_HEAD_OUTPUT_NAME}={self.head}\n")


def _detect_from_pull_request_event(
    ev: github_event.GitHubEvent,
) -> References | None:
    head = "HEAD"
    if ev.pull_request and ev.pull_request.head:
        head = ev.pull_request.head.sha

    # 0a) Merge-queue info via git note (published by the engine for newer MQs).
    # Falls back to the PR-body parsing below when the note is absent.
    if ev.pull_request and ev.pull_request.head and ev.pull_request.head.ref:
        note = queue_notes.read_mq_info_note(
            ev.pull_request.head.ref,
            ev.pull_request.head.sha,
        )
        if note is not None:
            return References(note["checking_base_sha"], head, "merge_queue")

    # 0b) merge-queue PR override
    content = queue_metadata.extract_from_event(ev)
    if content:
        return References(content["checking_base_sha"], head, "merge_queue")

    # 1) standard event payload
    if ev.pull_request and ev.pull_request.base:
        return References(ev.pull_request.base.sha, head, "github_event_pull_request")

    # 2) repository default branch fallback
    if ev.repository and ev.repository.default_branch:
        return References(
            ev.repository.default_branch,
            head,
            "github_event_pull_request",
        )

    return None


def _detect_from_push_event(ev: github_event.GitHubEvent) -> References | None:
    head_sha = ev.after or "HEAD"
    if ev.before:
        return References(ev.before, head_sha, "github_event_push")

    if ev.repository and ev.repository.default_branch:
        return References(ev.repository.default_branch, "HEAD", "github_event_push")

    return None


def _detect_from_buildkite() -> References | None:
    """Detect base/head references from Buildkite environment variables."""
    pr = os.getenv("BUILDKITE_PULL_REQUEST")
    if not pr or pr == "false":
        return None

    commit = os.getenv("BUILDKITE_COMMIT", "HEAD")
    branch = os.getenv("BUILDKITE_BRANCH")

    # Merge-queue info via git note (published by the engine). When present,
    # overrides the standard PR base branch so scope detection compares
    # against the MQ checking base rather than the target branch.
    if branch:
        note = queue_notes.read_mq_info_note(branch, commit)
        if note is not None:
            return References(note["checking_base_sha"], commit, "merge_queue")

    base_branch = os.getenv("BUILDKITE_PULL_REQUEST_BASE_BRANCH")
    if base_branch:
        return References(
            base_branch,
            commit,
            "buildkite_pull_request",
        )
    return None


def detect() -> References:
    # Try Buildkite-specific detection first
    if os.getenv("BUILDKITE") == "true":
        result = _detect_from_buildkite()
        if result:
            return result

    try:
        event_name, event = utils.get_github_event()
    except utils.GitHubEventNotFoundError:
        # fallback to last commit
        return References("HEAD^", "HEAD", "fallback_last_commit")

    if event_name in queue_metadata.PULL_REQUEST_EVENTS:
        result = _detect_from_pull_request_event(event)
        if result:
            return result

    elif event_name == "push":
        result = _detect_from_push_event(event)
        if result:
            return result

    else:
        return References(None, "HEAD", "github_event_other")

    msg = "Could not detect base SHA. Provide GITHUB_EVENT_NAME / GITHUB_EVENT_PATH."
    raise BaseNotFoundError(msg)
