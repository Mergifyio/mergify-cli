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
import os
import pathlib
import sys
import tempfile
import typing

from mergify_cli import console
from mergify_cli import utils
from mergify_cli.stack import changes
from mergify_cli.stack.push import LocalBranchInvalidError
from mergify_cli.stack.push import check_local_branch


@dataclasses.dataclass
class MergedCommit:
    """A commit whose PR has been merged."""

    commit_sha: str
    title: str
    pull_number: int
    pull_url: str


@dataclasses.dataclass
class RemainingCommit:
    """A commit that still has an open PR or no PR yet."""

    commit_sha: str
    title: str


@dataclasses.dataclass
class SyncStatus:
    """Result of a sync status check for the current stack."""

    branch: str
    trunk: str
    merged: list[MergedCommit]
    remaining: list[RemainingCommit]

    @property
    def all_merged(self) -> bool:
        """True when every commit in the stack has been merged."""
        return bool(self.merged) and not self.remaining

    @property
    def up_to_date(self) -> bool:
        """True when there are no merged commits (nothing to rebase away)."""
        return not self.merged


async def get_sync_status(
    github_server: str,
    token: str,
    *,
    trunk: tuple[str, str],
    branch_prefix: str | None = None,
    author: str | None = None,
) -> SyncStatus:
    """Compute the sync status for the current stack.

    Args:
        github_server: GitHub API server URL
        token: GitHub personal access token
        trunk: Tuple of (remote, branch) for the trunk
        branch_prefix: Optional branch prefix for stack branches
        author: Optional author filter (defaults to token owner)

    Returns:
        SyncStatus classifying each commit as merged or remaining
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
            "You should run `mergify stack sync` on the branch you created in the first place",
        )
        sys.exit(1)

    remote, base_branch = trunk

    user, repo = utils.get_slug(
        await utils.git("config", "--get", f"remote.{remote}.url"),
    )

    if base_branch == dest_branch:
        remote_url = await utils.git("remote", "get-url", remote)
        console.print(
            f"Your local branch `{dest_branch}` targets itself: "
            f"`{remote}/{base_branch}` (at {remote_url}@{base_branch}).\n"
            "You should either fix the target branch or rename your local branch.\n\n"
            f"* To fix the target branch: "
            f"`git branch {dest_branch} --set-upstream-to={remote}/{base_branch}`\n"
            f"* To rename your local branch: "
            f"`git branch -M {dest_branch} new-branch-name`",
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

    merged: list[MergedCommit] = []
    remaining: list[RemainingCommit] = []

    for local_change in stack_changes.locals:
        if local_change.action == "skip-merged":
            pull = local_change.pull
            merged.append(
                MergedCommit(
                    commit_sha=local_change.commit_sha,
                    title=local_change.title,
                    pull_number=int(pull["number"]) if pull else 0,
                    pull_url=pull["html_url"] if pull else "",
                ),
            )
        else:
            remaining.append(
                RemainingCommit(
                    commit_sha=local_change.commit_sha,
                    title=local_change.title,
                ),
            )

    return SyncStatus(
        branch=dest_branch,
        trunk=f"{remote}/{base_branch}",
        merged=merged,
        remaining=remaining,
    )


def _write_drop_script(merged_shas: set[str]) -> pathlib.Path:
    """Write a temporary bash script that acts as GIT_SEQUENCE_EDITOR to drop merged commits.

    The script reads the rebase todo file, replaces "pick <sha>" with
    "drop <sha>" for each merged commit, and writes it back.

    Returns the path to the script. Caller is responsible for cleanup.
    """
    short_shas = sorted(sha[:7] for sha in merged_shas)

    # Build sed expressions — one per merged SHA
    sed_expressions = " ".join(f"-e 's/^pick {sha}/drop {sha}/'" for sha in short_shas)

    script = f'#!/bin/sh\nsed {sed_expressions} "$1" > "$1.tmp" && mv "$1.tmp" "$1"\n'

    fd, path = tempfile.mkstemp(suffix=".sh", prefix="mergify_drop_")
    script_path = pathlib.Path(path)
    os.close(fd)
    script_path.write_text(script, encoding="utf-8")
    script_path.chmod(0o755)
    return script_path


async def smart_rebase(
    github_server: str,
    token: str,
    *,
    trunk: tuple[str, str],
    branch_prefix: str | None = None,
    author: str | None = None,
) -> SyncStatus:
    """Rebase the stack onto trunk, dropping any merged commits.

    If merged commits are found, does a single `git rebase -i` that both
    drops them and rebases onto the latest trunk. Otherwise, falls back
    to a simple `git pull --rebase`.

    Callers are responsible for fetching the remote before calling this.

    Returns the SyncStatus so callers can inspect what happened.
    """
    remote, base_branch = trunk

    status = await get_sync_status(
        github_server,
        token,
        trunk=trunk,
        branch_prefix=branch_prefix,
        author=author,
    )

    if status.all_merged or status.up_to_date:
        # Simple rebase — no merged commits to drop
        await utils.git("pull", "--rebase", remote, base_branch)
        return status

    # Merged commits found — drop them and rebase in one operation.
    # Using git rebase -i onto trunk with a script that changes "pick" to "drop"
    # for merged commits. This avoids conflicts from trying to reapply commits
    # whose content was modified on GitHub before merge.
    merged_shas = {m.commit_sha for m in status.merged}
    script_path = _write_drop_script(merged_shas)

    env_backup = os.environ.get("GIT_SEQUENCE_EDITOR")
    os.environ["GIT_SEQUENCE_EDITOR"] = str(script_path)
    try:
        await utils.git("rebase", "-i", f"{remote}/{base_branch}")
    finally:
        if env_backup is None:
            os.environ.pop("GIT_SEQUENCE_EDITOR", None)
        else:
            os.environ["GIT_SEQUENCE_EDITOR"] = env_backup
        script_path.unlink(missing_ok=True)

    return status


async def stack_sync(
    github_server: str,
    token: str,
    *,
    trunk: tuple[str, str],
    dry_run: bool = False,
    branch_prefix: str | None = None,
    author: str | None = None,
) -> None:
    """Sync the current stack by removing merged commits and rebasing.

    Args:
        github_server: GitHub API server URL
        token: GitHub personal access token
        trunk: Tuple of (remote, branch) for the trunk
        dry_run: If True, only report what would be done
        branch_prefix: Optional branch prefix for stack branches
        author: Optional author filter (defaults to token owner)
    """
    remote, base_branch = trunk

    # Dry-run: just check status and report
    if dry_run:
        with console.status("Checking sync status\u2026"):
            status = await get_sync_status(
                github_server,
                token,
                trunk=trunk,
                branch_prefix=branch_prefix,
                author=author,
            )

        if status.all_merged:
            console.print(
                f"All commits in the stack have been merged into {base_branch}.\n"
                f"You can switch to {base_branch} with: git checkout {base_branch}",
            )
        elif status.up_to_date:
            console.print("Stack is up to date.")
        else:
            console.print(
                "[bold]Dry run:[/] the following merged commits would be removed:",
            )
            for m in status.merged:
                console.print(f"  - {m.title} (#{m.pull_number}, merged)")
            console.print(
                f"\n{len(status.remaining)} commit(s) would remain in the stack.",
            )
        return

    # Fetch and sync
    with console.status(f"Fetching {remote}/{base_branch}\u2026"):
        await utils.git("fetch", remote, base_branch)

    with console.status(f"Rebasing onto {remote}/{base_branch}\u2026"):
        status = await smart_rebase(
            github_server,
            token,
            trunk=trunk,
            branch_prefix=branch_prefix,
            author=author,
        )

    if status.all_merged:
        console.print(
            f"All commits in the stack have been merged into {base_branch}.\n"
            f"You can switch to {base_branch} with: git checkout {base_branch}",
        )
    elif status.up_to_date:
        console.print("Stack is up to date.")
    else:
        for m in status.merged:
            console.print(f"  ✓ Dropped: {m.title} (#{m.pull_number})")
        console.print(
            f"Dropped {len(status.merged)} merged commit(s). "
            f"{len(status.remaining)} commit(s) remaining in the stack.",
        )
