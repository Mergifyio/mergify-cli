from __future__ import annotations

import os

from mergify_cli import console
from mergify_cli import utils
from mergify_cli.stack.reorder import get_stack_commits
from mergify_cli.stack.reorder import match_commit
from mergify_cli.stack.reorder import run_scripted_rebase


async def stack_edit(commit_prefix: str | None = None) -> None:
    os.chdir(await utils.git("rev-parse", "--show-toplevel"))
    trunk = await utils.get_trunk()
    base = await utils.git("merge-base", trunk, "HEAD")

    if commit_prefix is None:
        os.execvp("git", ("git", "rebase", "-i", base))  # noqa: S606
    else:
        commits = get_stack_commits(base)
        if not commits:
            console.print("No commits in the stack", style="green")
            return

        sha, subject, _ = match_commit(commit_prefix, commits)
        console.print(f"Editing commit: {sha[:12]} {subject}")
        _run_edit_rebase(base, sha)
        console.print(
            "Amend the commit, then run: git rebase --continue",
        )


def _run_edit_rebase(base: str, target_sha: str) -> None:
    """Run ``git rebase -i`` marking *target_sha* as ``edit``."""
    script_content = (
        "import sys\n"
        "target = " + repr(target_sha) + "\n"
        "todo_path = sys.argv[1]\n"
        "with open(todo_path) as f:\n"
        "    lines = f.readlines()\n"
        "result = []\n"
        "for line in lines:\n"
        "    parts = line.split(None, 2)\n"
        "    if len(parts) >= 2 and parts[0] == 'pick':\n"
        "        if target.startswith(parts[1]) or parts[1].startswith(target):\n"
        "            line = 'edit' + line[4:]\n"
        "    result.append(line)\n"
        "with open(todo_path, 'w') as f:\n"
        "    f.writelines(result)\n"
    )
    run_scripted_rebase(base, script_content)
