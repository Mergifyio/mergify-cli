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
import pathlib
import shlex
import tempfile

from mergify_cli import console
from mergify_cli import utils
from mergify_cli.stack.reorder import display_action_plan
from mergify_cli.stack.reorder import get_stack_commits
from mergify_cli.stack.reorder import match_commit
from mergify_cli.stack.reorder import run_action_rebase


async def stack_reword(
    commit_prefix: str,
    *,
    message: str | None,
    dry_run: bool,
) -> None:
    os.chdir(await utils.git("rev-parse", "--show-toplevel"))
    trunk = await utils.get_trunk()
    base = await utils.git("merge-base", trunk, "HEAD")
    commits = get_stack_commits(base)

    if not commits:
        console.print("No commits in the stack", style="green")
        return

    target_sha, _, _ = match_commit(commit_prefix, commits)
    current_shas = [c[0] for c in commits]

    # The rebase action used differs depending on whether -m was provided
    # (see below); reflect that in the displayed plan.
    plan_action = "reword" if message is None else "amend"
    display_action_plan("Reword plan:", commits, {target_sha: plan_action})

    if dry_run:
        console.print("Dry run — no changes made", style="green")
        return

    if message is None:
        # Mark target as `reword`: git stops at that commit and runs
        # `git commit --amend`, which opens $GIT_EDITOR. Works in a TTY;
        # hangs in agent contexts. Pass -m to stay non-interactive.
        run_action_rebase(base, current_shas, {target_sha: "reword"})
    else:
        # Keep the target as `pick` and inject `exec git commit --amend
        # -F <file>` immediately after. The amend runs while HEAD is
        # the target commit, so prepare-commit-msg re-attaches the
        # Change-Id. The message is passed via a temp file (not
        # `-m "..."`) so multi-line messages survive embedding into a
        # single rebase-todo line.
        msg_fd, msg_path = tempfile.mkstemp(suffix=".txt", prefix="mergify_reword_msg_")
        with os.fdopen(msg_fd, "w") as f:
            f.write(message)
        # Intentionally NOT in a `finally`: if `run_action_rebase` raises
        # SystemExit (conflicts), the rebase-todo still references this
        # file, so `git rebase --continue` will need it to complete the
        # exec amend. Leak the file rather than break --continue; the
        # system temp directory is cleaned up by the OS.
        run_action_rebase(
            base,
            current_shas,
            {},
            exec_after_sha=target_sha,
            exec_command=f"git commit --amend -F {shlex.quote(msg_path)}",
        )
        pathlib.Path(msg_path).unlink(missing_ok=True)

    console.print("Commit reworded successfully.", style="green")
