from __future__ import annotations

import dataclasses
import os
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


@dataclasses.dataclass
class Base:
    ref: str
    is_merge_queue: bool


def detect() -> Base:
    try:
        event = utils.get_github_event()
    except utils.GitHubEventNotFoundError:
        pass
    else:
        # 0) merge-queue PR override
        mq_sha = _detect_base_from_merge_queue_payload(event)
        if mq_sha:
            return Base(mq_sha, is_merge_queue=True)

        # 1) standard event payload
        event_sha = _detect_base_from_event(event)
        if event_sha:
            return Base(event_sha, is_merge_queue=False)

    # 2) base ref (e.g., PR target branch)
    base_ref = os.environ.get("GITHUB_BASE_REF")
    if base_ref:
        return Base(base_ref, is_merge_queue=False)

    msg = "Could not detect base SHA. Provide GITHUB_EVENT_PATH / GITHUB_BASE_REF."
    raise BaseNotFoundError(msg)
