from __future__ import annotations

import dataclasses
import os
import pathlib
import typing

import yaml

from mergify_cli import utils
from mergify_cli.ci.scopes import exceptions


GITHUB_ACTIONS_BASE_OUTPUT_NAME = "base"
GITHUB_ACTIONS_HEAD_OUTPUT_NAME = "head"


class BaseNotFoundError(exceptions.ScopesError):
    pass


class MergeQueuePullRequest(typing.TypedDict):
    number: int


class MergeQueueBatchFailed(typing.TypedDict):
    draft_pr_number: int
    checked_pull_request: list[int]


class MergeQueueMetadata(typing.TypedDict):
    checking_base_sha: str
    pull_requests: list[MergeQueuePullRequest]
    previous_failed_batches: list[MergeQueueBatchFailed]


def _yaml_docs_from_fenced_blocks(body: str) -> MergeQueueMetadata | None:
    lines = []
    found = False
    for line in body.splitlines():
        if line.startswith("```yaml"):
            found = True
        elif found:
            if line.startswith("```"):
                break
            lines.append(line)
    if lines:
        return typing.cast("MergeQueueMetadata", yaml.safe_load("\n".join(lines)))
    return None


def _detect_base_from_merge_queue_payload(ev: dict[str, typing.Any]) -> str | None:
    pr = ev.get("pull_request")
    if not isinstance(pr, dict):
        return None
    title = pr.get("title") or ""
    if not isinstance(title, str):
        return None
    if not title.startswith("merge-queue: "):
        return None
    body = pr.get("body") or ""
    content = _yaml_docs_from_fenced_blocks(body)
    if content:
        return content["checking_base_sha"]
    return None


def _detect_head_from_event(ev: dict[str, typing.Any]) -> str | None:
    pr = ev.get("pull_request")
    if isinstance(pr, dict):
        sha = pr.get("head", {}).get("sha")
        if isinstance(sha, str) and sha:
            return sha

    return None


def _detect_base_from_event(ev: dict[str, typing.Any]) -> str | None:
    pr = ev.get("pull_request")
    if isinstance(pr, dict):
        sha = pr.get("base", {}).get("sha")
        if isinstance(sha, str) and sha:
            return sha
    return None


def _detect_default_branch_from_event(ev: dict[str, typing.Any]) -> str | None:
    repo = ev.get("repository")
    if isinstance(repo, dict):
        sha = repo.get("default_branch")
        if isinstance(sha, str) and sha:
            return sha
    return None


def _detect_head_from_push_event(ev: dict[str, typing.Any]) -> str | None:
    sha = ev.get("after")
    if isinstance(sha, str) and sha:
        return sha
    return None


def _detect_base_from_push_event(ev: dict[str, typing.Any]) -> str | None:
    sha = ev.get("before")
    if isinstance(sha, str) and sha:
        return sha
    return None


ReferencesSource = typing.Literal[
    "manual",
    "merge_queue",
    "fallback_last_commit",
    "github_event_other",
    "github_event_pull_request",
    "github_event_push",
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


PULL_REQUEST_EVENTS = {
    "pull_request",
    "pull_request_review",
    "pull_request_review_comment",
    "pull_request_target",
}


def detect() -> References:
    try:
        event_name, event = utils.get_github_event()
    except utils.GitHubEventNotFoundError:
        # fallback to last commit
        return References("HEAD^", "HEAD", "fallback_last_commit")
    else:
        if event_name in PULL_REQUEST_EVENTS:
            head = _detect_head_from_event(event) or "HEAD"
            # 0) merge-queue PR override
            mq_sha = _detect_base_from_merge_queue_payload(event)
            if mq_sha:
                return References(mq_sha, head, "merge_queue")

            # 1) standard event payload
            event_sha = _detect_base_from_event(event)
            if event_sha:
                return References(event_sha, head, "github_event_pull_request")

            # 2) standard event payload
            event_sha = _detect_default_branch_from_event(event)
            if event_sha:
                return References(
                    event_sha,
                    head,
                    "github_event_pull_request",
                )

        elif event_name == "push":
            head_sha = _detect_head_from_push_event(event) or "HEAD"
            base_sha = _detect_base_from_push_event(event)
            if base_sha:
                return References(base_sha, head_sha, "github_event_push")

            event_sha = _detect_default_branch_from_event(event)
            if event_sha:
                return References(event_sha, "HEAD", "github_event_push")

        else:
            return References(None, "HEAD", "github_event_other")

    msg = "Could not detect base SHA. Provide GITHUB_EVENT_NAME / GITHUB_EVENT_PATH."
    raise BaseNotFoundError(msg)
