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

import dataclasses
import json
import sys
import typing

from mergify_cli import console
from mergify_cli import utils
from mergify_cli.stack import changes
from mergify_cli.stack.push import LocalBranchInvalidError
from mergify_cli.stack.push import check_local_branch


StackEntryStatusT = typing.Literal["merged", "draft", "open", "no_pr"]

_STATUS_DISPLAY: dict[StackEntryStatusT, tuple[str, str]] = {
    "merged": ("merged", "purple"),
    "draft": ("draft", "yellow"),
    "open": ("open", "green"),
    "no_pr": ("no PR", "dim"),
}

if typing.TYPE_CHECKING:
    from mergify_cli import github_types


@dataclasses.dataclass
class StackListEntry:
    """A single entry in the stack list."""

    commit_sha: str
    title: str
    change_id: str
    status: StackEntryStatusT
    pull_number: int | None = None
    pull_url: str | None = None

    def to_dict(self) -> dict[str, typing.Any]:
        return {
            "commit_sha": self.commit_sha,
            "title": self.title,
            "change_id": self.change_id,
            "status": self.status,
            "pull_number": self.pull_number,
            "pull_url": self.pull_url,
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


def display_stack_list(output: StackListOutput) -> None:
    """Display the stack list in human-readable format using rich console."""
    console.print(
        f"\nStack on `[cyan]{output.branch}[/]` targeting `[cyan]{output.trunk}[/]`:\n",
    )

    if not output.entries:
        console.print("No commits in stack", style="dim")
        return

    for entry in output.entries:
        status_text, status_color = _get_status_display(entry.status)
        short_sha = entry.commit_sha[:7]

        # Format: * [status] #number Title (sha)
        if entry.pull_number is not None:
            console.print(
                f"* [{status_color}]\\[{status_text}][/] "
                f"[bold]#{entry.pull_number}[/] {entry.title} ({short_sha})",
            )
            console.print(f"  {entry.pull_url}\n")
        else:
            console.print(
                f"* [{status_color}]\\[{status_text}][/] {entry.title} ({short_sha})\n",
            )


async def get_stack_list(
    github_server: str,
    token: str,
    *,
    trunk: tuple[str, str],
    branch_prefix: str | None = None,
    author: str | None = None,
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
        console.print(f"[red] {e.message} [/]")
        console.print(
            "You should run `mergify stack list` on the branch you created in the first place",
        )
        sys.exit(1)

    remote, base_branch = trunk

    user, repo = utils.get_slug(
        await utils.git("config", "--get", f"remote.{remote}.url"),
    )

    if base_branch == dest_branch:
        remote_url = await utils.git("remote", "get-url", remote)
        console.print(
            f"Your local branch `{dest_branch}` targets itself: `{remote}/{base_branch}` (at {remote_url}@{base_branch}).\n"
            f"You should either fix the target branch or rename your local branch.\n\n"
            f"* To fix the target branch: `git branch {dest_branch} --set-upstream-to={remote}/main>\n",
            f"* To rename your local branch: `git branch -M {dest_branch} new-branch-name`",
            style="red",
        )
        sys.exit(1)

    stack_prefix = f"{branch_prefix}/{dest_branch}" if branch_prefix else dest_branch

    base_commit_sha = await utils.git(
        "merge-base",
        "--fork-point",
        f"{remote}/{base_branch}",
    )
    if not base_commit_sha:
        console.print(
            f"Common commit between `{remote}/{base_branch}` and `{dest_branch}` branches not found",
            style="red",
        )
        sys.exit(1)

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
    for local_change in stack_changes.locals:
        status = _get_entry_status(local_change.pull)
        entry = StackListEntry(
            commit_sha=local_change.commit_sha,
            title=local_change.title,
            change_id=local_change.id,
            status=status,
            pull_number=int(local_change.pull["number"]) if local_change.pull else None,
            pull_url=local_change.pull["html_url"] if local_change.pull else None,
        )
        entries.append(entry)

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
) -> None:
    """List the current stack's commits and their associated PRs.

    Args:
        github_server: GitHub API server URL
        token: GitHub personal access token
        trunk: Tuple of (remote, branch) for the trunk
        branch_prefix: Optional branch prefix for stack branches
        author: Optional author filter (defaults to token owner)
        output_json: If True, output JSON instead of human-readable format
    """
    output = await get_stack_list(
        github_server=github_server,
        token=token,
        trunk=trunk,
        branch_prefix=branch_prefix,
        author=author,
    )

    if output_json:
        console.print(json.dumps(output.to_dict(), indent=2))
    else:
        display_stack_list(output)
