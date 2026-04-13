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

import os
import sys

from mergify_cli import console
from mergify_cli import utils
from mergify_cli.stack.reorder import display_plan
from mergify_cli.stack.reorder import get_stack_commits
from mergify_cli.stack.reorder import match_commit
from mergify_cli.stack.reorder import run_rebase


async def stack_move(
    commit_prefix: str,
    position: str,
    target_prefix: str | None,
    *,
    dry_run: bool,
) -> None:
    os.chdir(await utils.git("rev-parse", "--show-toplevel"))
    trunk = await utils.get_trunk()
    base = await utils.git("merge-base", trunk, "HEAD")
    commits = get_stack_commits(base)

    if not commits:
        console.print("No commits in the stack", style="green")
        return

    commit = match_commit(commit_prefix, commits)

    if position in {"before", "after"}:
        if target_prefix is None:
            console.print(
                f"error: '{position}' requires a target commit",
                style="red",
            )
            sys.exit(1)
        target = match_commit(target_prefix, commits)
        if commit[0] == target[0]:
            console.print(
                "error: commit and target are the same",
                style="red",
            )
            sys.exit(1)
    elif position in {"first", "last"}:
        if target_prefix is not None:
            console.print(
                f"error: '{position}' does not accept a target commit",
                style="red",
            )
            sys.exit(1)

    # Compute new order
    remaining = [c for c in commits if c[0] != commit[0]]

    if position == "first":
        new_order = [commit, *remaining]
    elif position == "last":
        new_order = [*remaining, commit]
    elif position == "before":
        target_idx = next(i for i, c in enumerate(remaining) if c[0] == target[0])
        new_order = [*remaining[:target_idx], commit, *remaining[target_idx:]]
    elif position == "after":
        target_idx = next(i for i, c in enumerate(remaining) if c[0] == target[0])
        new_order = [
            *remaining[: target_idx + 1],
            commit,
            *remaining[target_idx + 1 :],
        ]

    # Check if order changed
    current_shas = [c[0] for c in commits]
    new_shas = [c[0] for c in new_order]
    if current_shas == new_shas:
        console.print(
            "Commit is already in the requested position",
            style="green",
        )
        return

    display_plan("Move plan:", new_order)

    if dry_run:
        console.print("Dry run — no changes made", style="green")
        return

    run_rebase(base, new_shas)
    console.print("Commit moved successfully", style="green")
