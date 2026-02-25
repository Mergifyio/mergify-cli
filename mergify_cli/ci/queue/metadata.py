from __future__ import annotations

import typing

import click
import yaml

from mergify_cli import utils


if typing.TYPE_CHECKING:
    from mergify_cli.ci import github_event


class MergeQueuePullRequest(typing.TypedDict):
    number: int


class MergeQueueBatchFailed(typing.TypedDict):
    draft_pr_number: int
    checked_pull_requests: list[int]


class MergeQueueMetadata(typing.TypedDict):
    checking_base_sha: str
    pull_requests: list[MergeQueuePullRequest]
    previous_failed_batches: list[MergeQueueBatchFailed]


PULL_REQUEST_EVENTS = {
    "pull_request",
    "pull_request_review",
    "pull_request_review_comment",
    "pull_request_target",
}


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


def extract_from_event(ev: github_event.GitHubEvent) -> MergeQueueMetadata | None:
    if ev.pull_request is None:
        return None
    if not ev.pull_request.title or not ev.pull_request.title.startswith(
        "merge queue: ",
    ):
        return None
    if not ev.pull_request.body:
        click.echo(
            "WARNING: MQ pull request without body, skipping metadata extraction",
            err=True,
        )
        return None
    ref = _yaml_docs_from_fenced_blocks(ev.pull_request.body)
    if ref is None:
        click.echo(
            "WARNING: MQ pull request body without Mergify metadata, skipping metadata extraction",
            err=True,
        )
    return ref


def detect() -> MergeQueueMetadata | None:
    """Detect and return merge queue metadata from the GitHub event payload.

    Returns None if not running in a merge queue context.
    """
    try:
        event_name, event = utils.get_github_event()
    except utils.GitHubEventNotFoundError:
        return None

    if event_name not in PULL_REQUEST_EVENTS:
        return None

    return extract_from_event(event)
