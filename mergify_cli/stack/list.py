#
#  Copyright © 2021-2026 Mergify SAS
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
import json
import sys
import typing

import click

from mergify_cli import console
from mergify_cli import console_error
from mergify_cli import utils
from mergify_cli.exit_codes import ExitCode
from mergify_cli.stack import changes
from mergify_cli.stack.push import LocalBranchInvalidError
from mergify_cli.stack.push import check_local_branch


StackEntryStatusT = typing.Literal["merged", "draft", "open", "no_pr"]
CIStatusT = typing.Literal["passing", "failing", "pending", "unknown"]
ReviewStatusT = typing.Literal["approved", "changes_requested", "pending", "unknown"]

_STATUS_DISPLAY: dict[StackEntryStatusT, tuple[str, str]] = {
    "merged": ("✓ merged", "purple"),
    "draft": ("● draft", "yellow"),
    "open": ("● open", "green"),
    "no_pr": ("○ no PR", "dim"),
}

_CI_STATUS_DISPLAY: dict[CIStatusT, tuple[str, str]] = {
    "passing": ("✓ passing", "green"),
    "failing": ("✗ failing", "red"),
    "pending": ("● pending", "yellow"),
    "unknown": ("—", "dim"),
}

_REVIEW_STATUS_DISPLAY: dict[ReviewStatusT, tuple[str, str]] = {
    "approved": ("✓ approved", "green"),
    "changes_requested": ("✗ changes requested", "red"),
    "pending": ("● pending", "yellow"),
    "unknown": ("—", "dim"),
}

if typing.TYPE_CHECKING:
    import httpx

    from mergify_cli import github_types


@dataclasses.dataclass
class CICheck:
    name: str
    status: str

    def to_dict(self) -> dict[str, str]:
        return {"name": self.name, "status": self.status}


@dataclasses.dataclass
class Review:
    user: str
    state: str

    def to_dict(self) -> dict[str, str]:
        return {"user": self.user, "state": self.state}


@dataclasses.dataclass
class StackListEntry:
    """A single entry in the stack list."""

    commit_sha: str
    title: str
    change_id: str
    status: StackEntryStatusT
    pull_number: int | None = None
    pull_url: str | None = None
    ci_status: CIStatusT = "unknown"
    ci_checks: list[CICheck] = dataclasses.field(default_factory=list)
    review_status: ReviewStatusT = "unknown"
    reviews: list[Review] = dataclasses.field(default_factory=list)
    mergeable: bool | None = None

    def to_dict(self) -> dict[str, typing.Any]:
        return {
            "commit_sha": self.commit_sha,
            "title": self.title,
            "change_id": self.change_id,
            "status": self.status,
            "pull_number": self.pull_number,
            "pull_url": self.pull_url,
            "ci_status": self.ci_status,
            "ci_checks": [c.to_dict() for c in self.ci_checks],
            "review_status": self.review_status,
            "reviews": [r.to_dict() for r in self.reviews],
            "mergeable": self.mergeable,
        }


@dataclasses.dataclass
class StackListOutput:
    """Output structure for the stack list command."""

    branch: str
    trunk: str
    entries: list[StackListEntry]

    def to_dict(self) -> dict[str, typing.Any]:
        return {
            "branch": self.branch,
            "trunk": self.trunk,
            "entries": [e.to_dict() for e in self.entries],
        }


def _format_ci_display(entry: StackListEntry, *, verbose: bool) -> str:
    if entry.ci_status == "unknown":
        return ""
    if verbose and entry.ci_checks:
        checks = []
        for check in entry.ci_checks:
            if check.status == "success":
                checks.append(f"[green]✓ {check.name}[/]")
            elif check.status == "failure":
                checks.append(f"[red]✗ {check.name}[/]")
            else:
                checks.append(f"[yellow]● {check.name}[/]")
        return f"CI: {', '.join(checks)}"
    text, color = _CI_STATUS_DISPLAY[entry.ci_status]
    return f"CI: [{color}]{text}[/]"


def _format_review_display(entry: StackListEntry, *, verbose: bool) -> str:
    if entry.review_status == "unknown":
        return ""
    if verbose and entry.reviews:
        reviewers = []
        for review in entry.reviews:
            if review.state == "APPROVED":
                reviewers.append(f"[green]✓ {review.user}[/]")
            elif review.state == "CHANGES_REQUESTED":
                reviewers.append(f"[red]✗ {review.user}[/]")
            else:
                reviewers.append(f"[dim]{review.user}[/]")
        return f"Review: {', '.join(reviewers)}"
    text, color = _REVIEW_STATUS_DISPLAY[entry.review_status]
    return f"Review: [{color}]{text}[/]"


def _get_entry_status(
    pull: github_types.PullRequest | None,
) -> StackEntryStatusT:
    """Determine the status of a stack entry based on its PR state."""
    if pull is None:
        return "no_pr"
    if pull["merged_at"]:
        return "merged"
    if pull["draft"]:
        return "draft"
    return "open"


def _get_status_display(status: StackEntryStatusT) -> tuple[str, str]:
    """Return the display text and color for a status."""
    return _STATUS_DISPLAY[status]


def _compute_ci_status(
    check_runs: list[dict[str, typing.Any]],
) -> tuple[CIStatusT, list[CICheck]]:
    """Compute CI status from GitHub check run data."""
    if not check_runs:
        return ("unknown", [])

    checks: list[CICheck] = []
    has_pending = False
    has_failure = False

    for run in check_runs:
        name = run.get("name", "")
        if run.get("status") != "completed":
            checks.append(CICheck(name=name, status="pending"))
            has_pending = True
        elif run.get("conclusion") in {"success", "skipped"}:
            checks.append(CICheck(name=name, status="success"))
        else:
            checks.append(CICheck(name=name, status="failure"))
            has_failure = True

    if has_failure:
        status: CIStatusT = "failing"
    elif has_pending:
        status = "pending"
    else:
        status = "passing"

    return (status, checks)


def _compute_review_status(
    reviews_data: list[dict[str, typing.Any]],
) -> tuple[ReviewStatusT, list[Review]]:
    """Compute review status from GitHub review data."""
    if not reviews_data:
        return ("unknown", [])

    # Keep latest review per user (APPROVED/CHANGES_REQUESTED/DISMISSED
    # take precedence over COMMENTED)
    latest_by_user: dict[str, str] = {}
    for review in reviews_data:
        user = review.get("user", {}).get("login", "")
        state = review.get("state", "")
        if not user:
            continue
        if (
            state in {"APPROVED", "CHANGES_REQUESTED", "DISMISSED"}
            or user not in latest_by_user
        ):
            latest_by_user[user] = state

    reviews = [Review(user=u, state=s) for u, s in latest_by_user.items()]

    has_changes_requested = any(r.state == "CHANGES_REQUESTED" for r in reviews)
    has_approved = any(r.state == "APPROVED" for r in reviews)

    if has_changes_requested:
        status: ReviewStatusT = "changes_requested"
    elif has_approved:
        status = "approved"
    else:
        status = "pending"

    return (status, reviews)


_MAX_CONCURRENT_API_CALLS = 5


async def _fetch_pr_details(
    client: httpx.AsyncClient,
    user: str,
    repo: str,
    entries: list[StackListEntry],
    pulls: dict[int, github_types.PullRequest],
) -> None:
    """Fetch CI checks and reviews for each PR and update entries in place."""
    sem = asyncio.Semaphore(_MAX_CONCURRENT_API_CALLS)

    async def _fetch_for_entry(entry: StackListEntry) -> None:
        if entry.pull_number is None:
            return

        pull = pulls.get(entry.pull_number)
        if pull is None:
            return

        head_sha = pull["head"]["sha"]

        async with sem:
            r_checks, r_reviews = await asyncio.gather(
                client.get(
                    f"/repos/{user}/{repo}/commits/{head_sha}/check-runs",
                ),
                client.get(
                    f"/repos/{user}/{repo}/pulls/{entry.pull_number}/reviews",
                ),
            )

        check_runs = r_checks.json().get("check_runs", [])
        entry.ci_status, entry.ci_checks = _compute_ci_status(check_runs)

        reviews_data = r_reviews.json()
        entry.review_status, entry.reviews = _compute_review_status(reviews_data)

    await asyncio.gather(*[_fetch_for_entry(e) for e in entries])


def display_stack_list(output: StackListOutput, *, verbose: bool = False) -> None:
    """Display the stack list in human-readable format using rich console."""
    console.print(
        f"\nStack on [cyan]{output.branch}[/] → [cyan]{output.trunk}[/]:\n",
    )

    if not output.entries:
        console.print("  No commits in stack", style="dim")
        return

    for entry in output.entries:
        status_text, status_color = _get_status_display(entry.status)
        short_sha = entry.commit_sha[:7]

        if entry.pull_number is not None:
            conflict = " [red]✗ conflicting[/]" if entry.mergeable is False else ""

            console.print(
                f"  [{status_color}]{status_text}[/] "
                f"[bold]#{entry.pull_number}[/] {entry.title} "
                f"[dim]({short_sha})[/]{conflict}",
            )

            # Status line (CI + review)
            ci_display = _format_ci_display(entry, verbose=verbose)
            review_display = _format_review_display(entry, verbose=verbose)
            parts = [p for p in [ci_display, review_display] if p]
            if parts:
                console.print(f"     {' | '.join(parts)}")

            console.print(f"     [dim]{entry.pull_url}[/]\n")
        else:
            console.print(
                f"  [{status_color}]{status_text}[/] {entry.title} "
                f"[dim]({short_sha})[/]\n",
            )


async def get_stack_list(
    github_server: str,
    token: str,
    *,
    trunk: tuple[str, str],
    branch_prefix: str | None = None,
    author: str | None = None,
    include_status: bool = True,
) -> StackListOutput:
    """Get the current stack's commits and their associated PRs.

    Args:
        github_server: GitHub API server URL
        token: GitHub personal access token
        trunk: Tuple of (remote, branch) for the trunk
        branch_prefix: Optional branch prefix for stack branches
        author: Optional author filter (defaults to token owner)

    Returns:
        StackListOutput containing branch info and list of entries
    """
    dest_branch = await utils.git_get_branch_name()

    if author is None:
        async with utils.get_github_http_client(github_server, token) as client:
            r_author = await client.get("/user")
            author = typing.cast("str", r_author.json()["login"])

    if branch_prefix is None:
        branch_prefix = await utils.get_default_branch_prefix(author)

    try:
        check_local_branch(branch_name=dest_branch, branch_prefix=branch_prefix)
    except LocalBranchInvalidError as e:
        console_error(e.message)
        console.print(
            "You should run `mergify stack list` on the branch you created in the first place",
        )
        sys.exit(ExitCode.INVALID_STATE)

    remote, base_branch = trunk

    user, repo = utils.get_slug(
        await utils.git("config", "--get", f"remote.{remote}.url"),
    )

    if base_branch == dest_branch:
        remote_url = await utils.git("remote", "get-url", remote)
        console_error(
            f"your local branch `{dest_branch}` targets itself: "
            f"`{remote}/{base_branch}` (at {remote_url}@{base_branch})",
        )
        console.print(
            "You should either fix the target branch or rename your local branch.\n\n"
            f"* To fix the target branch: `git branch {dest_branch} --set-upstream-to={remote}/main`\n"
            f"* To rename your local branch: `git branch -M {dest_branch} new-branch-name`",
        )
        sys.exit(ExitCode.INVALID_STATE)

    stack_prefix = f"{branch_prefix}/{dest_branch}" if branch_prefix else dest_branch

    base_commit_sha = await utils.git(
        "merge-base",
        "--fork-point",
        f"{remote}/{base_branch}",
    )
    if not base_commit_sha:
        console_error(
            f"common commit between `{remote}/{base_branch}` and `{dest_branch}` branches not found",
        )
        sys.exit(ExitCode.STACK_NOT_FOUND)

    async with utils.get_github_http_client(github_server, token) as client:
        remote_changes = await changes.get_remote_changes(
            client,
            user,
            repo,
            stack_prefix,
            author,
        )

        stack_changes = await changes.get_changes(
            base_commit_sha=base_commit_sha,
            stack_prefix=stack_prefix,
            base_branch=base_branch,
            dest_branch=dest_branch,
            remote_changes=remote_changes,
            only_update_existing_pulls=False,
            next_only=False,
        )

        # Build output structure
        entries: list[StackListEntry] = []
        pulls_by_number: dict[int, github_types.PullRequest] = {}
        for local_change in stack_changes.locals:
            status = _get_entry_status(local_change.pull)
            pull_number = (
                int(local_change.pull["number"]) if local_change.pull else None
            )
            entry = StackListEntry(
                commit_sha=local_change.commit_sha,
                title=local_change.title,
                change_id=local_change.id,
                status=status,
                pull_number=pull_number,
                pull_url=local_change.pull["html_url"] if local_change.pull else None,
                mergeable=local_change.pull.get("mergeable")
                if local_change.pull
                else None,
            )
            entries.append(entry)
            if pull_number is not None and local_change.pull is not None:
                pulls_by_number[pull_number] = local_change.pull

        if include_status:
            await _fetch_pr_details(client, user, repo, entries, pulls_by_number)

    return StackListOutput(
        branch=dest_branch,
        trunk=f"{remote}/{base_branch}",
        entries=entries,
    )


async def stack_list(
    github_server: str,
    token: str,
    *,
    trunk: tuple[str, str],
    branch_prefix: str | None = None,
    author: str | None = None,
    output_json: bool = False,
    verbose: bool = False,
) -> None:
    """List the current stack's commits and their associated PRs.

    Args:
        github_server: GitHub API server URL
        token: GitHub personal access token
        trunk: Tuple of (remote, branch) for the trunk
        branch_prefix: Optional branch prefix for stack branches
        author: Optional author filter (defaults to token owner)
        output_json: If True, output JSON instead of human-readable format
        verbose: If True, show detailed CI check names and reviewer names
    """
    output = await get_stack_list(
        github_server=github_server,
        token=token,
        trunk=trunk,
        branch_prefix=branch_prefix,
        author=author,
    )

    if output_json:
        click.echo(json.dumps(output.to_dict(), indent=2))
    else:
        display_stack_list(output, verbose=verbose)
