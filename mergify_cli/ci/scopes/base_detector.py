from __future__ import annotations

import dataclasses
import typing

import yaml

from mergify_cli import utils
from mergify_cli.ci.scopes import exceptions


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


def _detect_base_from_push_event(ev: dict[str, typing.Any]) -> str | None:
    sha = ev.get("before")
    if isinstance(sha, str) and sha:
        return sha
    return None


@dataclasses.dataclass
class Base:
    ref: str
    is_merge_queue: bool


PULL_REQUEST_EVENTS = {
    "pull_request",
    "pull_request_review",
    "pull_request_review_comment",
    "pull_request_target",
}


def detect() -> Base:
    try:
        event_name, event = utils.get_github_event()
    except utils.GitHubEventNotFoundError:
        # fallback to last commit
        return Base("HEAD^", is_merge_queue=False)
    else:
        if event_name in PULL_REQUEST_EVENTS:
            # 0) merge-queue PR override
            mq_sha = _detect_base_from_merge_queue_payload(event)
            if mq_sha:
                return Base(mq_sha, is_merge_queue=True)

            # 1) standard event payload
            event_sha = _detect_base_from_event(event)
            if event_sha:
                return Base(event_sha, is_merge_queue=False)

            # 2) standard event payload
            event_sha = _detect_default_branch_from_event(event)
            if event_sha:
                return Base(event_sha, is_merge_queue=False)

        elif event_name == "push":
            event_sha = _detect_base_from_push_event(event)
            if event_sha:
                return Base(event_sha, is_merge_queue=False)

            event_sha = _detect_default_branch_from_event(event)
            if event_sha:
                return Base(event_sha, is_merge_queue=False)
        else:
            msg = "Unhandled GITHUB_EVENT_NAME"
            raise BaseNotFoundError(msg)

    msg = "Could not detect base SHA. Provide GITHUB_EVENT_NAME / GITHUB_EVENT_PATH."
    raise BaseNotFoundError(msg)
