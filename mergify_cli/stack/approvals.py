#
#  Copyright © 2026 Mergify SAS
#
# Licensed under the Apache License, Version 2.0 (the "License"); you may
# not use this file except in compliance with the License. You may obtain
# a copy of the License at
#
#      http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS, WITHOUT
# WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied. See the
# License for the specific language governing permissions and limitations
# under the License.
from __future__ import annotations

import asyncio
import dataclasses
import enum
import typing

import httpx


if typing.TYPE_CHECKING:
    from mergify_cli import github_types
    from mergify_cli.stack import changes


MAX_CONCURRENT_API_CALLS = 5


async def _pull_is_approved(
    client: httpx.AsyncClient,
    user: str,
    repo: str,
    pull_number: int,
    sem: asyncio.Semaphore,
) -> bool:
    async with sem:
        r = await client.get(f"/repos/{user}/{repo}/pulls/{pull_number}/reviews")
        r.raise_for_status()
        reviews = r.json()

    latest_by_reviewer: dict[str, str] = {}
    for review in reviews:
        state = review["state"]
        if state in {"COMMENTED", "PENDING"}:
            continue
        login = (review.get("user") or {}).get("login")
        if not login:
            continue
        latest_by_reviewer[login] = state

    return any(state == "APPROVED" for state in latest_by_reviewer.values())


async def fetch_approved_pull_numbers(
    client: httpx.AsyncClient,
    user: str,
    repo: str,
    pulls: list[github_types.PullRequest],
) -> set[int]:
    if not pulls:
        return set()

    sem = asyncio.Semaphore(MAX_CONCURRENT_API_CALLS)
    numbers = [int(pull["number"]) for pull in pulls]
    results = await asyncio.gather(
        *(_pull_is_approved(client, user, repo, n, sem) for n in numbers),
    )
    return {n for n, approved in zip(numbers, results, strict=True) if approved}


CONFLICT_STATE = "dirty"
_MERGEABLE_RETRY_DELAY_SECONDS = 1.0


async def bottom_pull_has_conflict(
    client: httpx.AsyncClient,
    user: str,
    repo: str,
    bottom_pull: github_types.PullRequest | None,
) -> bool:
    if bottom_pull is None:
        return False

    pull_number = int(bottom_pull["number"])
    url = f"/repos/{user}/{repo}/pulls/{pull_number}"

    try:
        r = await client.get(url)
        r.raise_for_status()
        data = r.json()

        if data.get("mergeable") is None:
            await asyncio.sleep(_MERGEABLE_RETRY_DELAY_SECONDS)
            r = await client.get(url)
            r.raise_for_status()
            data = r.json()
    except httpx.HTTPError:
        return False

    return bool(data.get("mergeable_state") == CONFLICT_STATE)


class RebaseReason(enum.Enum):
    EXPLICIT_SKIP = "explicit_skip"
    FORCED = "forced"
    CONFLICT_OVERRIDE = "conflict_override"
    SKIPPED_FOR_APPROVALS = "skipped_for_approvals"
    NO_APPROVALS = "no_approvals"


@dataclasses.dataclass
class RebaseDecision:
    should_rebase: bool
    reason: RebaseReason
    approved_pulls: list[github_types.PullRequest]


async def decide_rebase(
    client: httpx.AsyncClient,
    user: str,
    repo: str,
    *,
    planned_changes: changes.Changes,
    skip_rebase: bool,
    force_rebase: bool,
) -> RebaseDecision:
    if skip_rebase:
        return RebaseDecision(
            should_rebase=False,
            reason=RebaseReason.EXPLICIT_SKIP,
            approved_pulls=[],
        )
    if force_rebase:
        return RebaseDecision(
            should_rebase=True,
            reason=RebaseReason.FORCED,
            approved_pulls=[],
        )

    # All live PRs in the stack (excluding already-merged ones). A rebase
    # force-pushes every branch in the stack, not just the ones whose local
    # commit currently differs from the remote head — `skip-up-to-date` PRs
    # become `update` once the stack gets rebased, and their approvals would
    # be dismissed too.
    stack_pulls = [
        change.pull
        for change in planned_changes.locals
        if change.action != "skip-merged" and change.pull is not None
    ]

    approved_numbers = await fetch_approved_pull_numbers(
        client,
        user,
        repo,
        stack_pulls,
    )
    approved_pulls = [p for p in stack_pulls if int(p["number"]) in approved_numbers]

    # Bottom PR = first live (non-merged) change in the stack. If it's a new
    # `create`, there's no existing PR to check. Otherwise its pull is what
    # can force a rebase via a dirty mergeable state.
    bottom_pull: github_types.PullRequest | None = None
    for change in planned_changes.locals:
        if change.action == "skip-merged":
            continue
        bottom_pull = change.pull
        break
    has_conflict = await bottom_pull_has_conflict(client, user, repo, bottom_pull)

    if approved_pulls and has_conflict:
        return RebaseDecision(
            should_rebase=True,
            reason=RebaseReason.CONFLICT_OVERRIDE,
            approved_pulls=approved_pulls,
        )
    if approved_pulls:
        return RebaseDecision(
            should_rebase=False,
            reason=RebaseReason.SKIPPED_FOR_APPROVALS,
            approved_pulls=approved_pulls,
        )
    return RebaseDecision(
        should_rebase=True,
        reason=RebaseReason.NO_APPROVALS,
        approved_pulls=[],
    )
