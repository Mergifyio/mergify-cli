from __future__ import annotations

import typing


if typing.TYPE_CHECKING:
    import httpx


class QueuePullRequestAuthor(typing.TypedDict):
    id: int
    login: str


class QueuePullRequest(typing.TypedDict, total=False):
    number: typing.Required[int]
    title: typing.Required[str]
    url: typing.Required[str]
    author: typing.Required[QueuePullRequestAuthor]
    queued_at: typing.Required[str]
    priority_alias: typing.Required[str]
    priority_rule_name: typing.Required[str]
    labels: typing.Required[list[str]]
    scopes: typing.Required[list[str]]
    estimated_merge_at: str | None


class QueueChecksSummary(typing.TypedDict):
    passed: int
    total: int


class QueueBatchStatus(typing.TypedDict):
    code: str


class QueueBatch(typing.TypedDict, total=False):
    id: typing.Required[str]
    name: typing.Required[str]
    status: typing.Required[QueueBatchStatus]
    started_at: typing.Required[str]
    estimated_merge_at: typing.Required[str]
    checks_summary: typing.Required[QueueChecksSummary]
    pull_requests: typing.Required[list[QueuePullRequest]]
    parent_ids: list[str]
    batch_filled_slots: int | None
    max_batch_slots: int | None
    batch_max_start_at: str | None
    scopes: list[str]
    sub_batches: list[typing.Any] | None


class QueuePause(typing.TypedDict):
    reason: str
    paused_at: str


class QueueStatusResponse(typing.TypedDict, total=False):
    batches: typing.Required[list[QueueBatch]]
    waiting_pull_requests: typing.Required[list[QueuePullRequest]]
    scope_queues: typing.Required[dict[str, typing.Any]]
    pause: QueuePause | None


async def get_queue_status(
    client: httpx.AsyncClient,
    repository: str,
    *,
    branch: str | None = None,
) -> QueueStatusResponse:
    params: dict[str, str] = {}
    if branch is not None:
        params["branch"] = branch
    response = await client.get(
        f"/v1/repos/{repository}/merge-queue/status",
        params=params,
    )
    return response.json()  # type: ignore[no-any-return]


class QueueRule(typing.TypedDict):
    name: str
    config: dict[str, typing.Any]


class QueueCheck(typing.TypedDict, total=False):
    name: typing.Required[str]
    description: typing.Required[str]
    url: str | None
    state: typing.Required[str]
    avatar_url: str | None


class QueueConditionEvaluation(typing.TypedDict, total=False):
    match: typing.Required[bool]
    label: typing.Required[str]
    description: str | None
    subconditions: list[QueueConditionEvaluation]
    evaluations: list[dict[str, typing.Any]]


class QueueMergeabilityCheck(typing.TypedDict, total=False):
    check_type: typing.Required[str]
    queue_pull_request_number: typing.Required[int]
    started_at: str | None
    ci_ended_at: str | None
    ci_state: typing.Required[str]
    state: typing.Required[str]
    checks: typing.Required[list[QueueCheck]]
    conditions_evaluation: QueueConditionEvaluation | None


class QueuePullResponse(typing.TypedDict, total=False):
    number: typing.Required[int]
    queued_at: typing.Required[str]
    estimated_time_of_merge: str | None
    position: typing.Required[int]
    priority_rule_name: typing.Required[str]
    queue_rule_name: typing.Required[str]
    checks_timeout_at: str | None
    queue_rule: typing.Required[QueueRule]
    mergeability_check: QueueMergeabilityCheck | None


async def get_queue_pull(
    client: httpx.AsyncClient,
    repository: str,
    pr_number: int,
) -> QueuePullResponse:
    response = await client.get(
        f"/v1/repos/{repository}/merge-queue/pull/{pr_number}",
    )
    return response.json()  # type: ignore[no-any-return]
