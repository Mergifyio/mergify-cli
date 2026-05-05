from __future__ import annotations

import typing


if typing.TYPE_CHECKING:
    import httpx


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
