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
from mergify_cli import console_error
from mergify_cli import utils
from mergify_cli.exit_codes import ExitCode
from mergify_cli.stack.reorder import display_action_plan
from mergify_cli.stack.reorder import get_stack_commits
from mergify_cli.stack.reorder import match_commit
from mergify_cli.stack.reorder import run_action_rebase


async def stack_fixup(
    commit_prefixes: list[str],
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

    matched = [match_commit(p, commits) for p in commit_prefixes]

    # Check for duplicates
    matched_shas = [c[0] for c in matched]
    if len(set(matched_shas)) != len(matched_shas):
        seen: set[str] = set()
        for prefix, sha in zip(commit_prefixes, matched_shas, strict=True):
            if sha in seen:
                console_error(
                    f"duplicate — prefix '{prefix}' resolves to the same commit as another prefix",
                )
                sys.exit(ExitCode.INVALID_STATE)
            seen.add(sha)

    # Each listed commit must have a parent inside the stack (not the first)
    first_sha = commits[0][0]
    for sha in matched_shas:
        if sha == first_sha:
            console_error(
                "cannot fixup the first commit of the stack — no parent in stack",
            )
            sys.exit(ExitCode.INVALID_STATE)

    actions = dict.fromkeys(matched_shas, "fixup")
    current_shas = [c[0] for c in commits]

    display_action_plan("Fixup plan:", commits, actions)

    if dry_run:
        console.print("Dry run — no changes made", style="green")
        return

    run_action_rebase(base, current_shas, actions)
    console.print("Commits squashed successfully.", style="green")


async def stack_squash(
    src_prefixes: list[str],
    target_prefix: str,
    *,
    message: str | None,
    dry_run: bool,
) -> None:
    # Placeholder — implemented in a later task.
    raise NotImplementedError
