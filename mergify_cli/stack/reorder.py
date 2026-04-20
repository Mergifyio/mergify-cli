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
import subprocess
import sys
import tempfile

from mergify_cli import console
from mergify_cli import console_error
from mergify_cli import utils
from mergify_cli.exit_codes import ExitCode
from mergify_cli.stack.changes import CHANGEID_RE
from mergify_cli.stack.changes import is_change_id_prefix


def get_stack_commits(base: str) -> list[tuple[str, str, str]]:
    """Return (full_sha, subject, change_id) tuples from base to HEAD.

    Uses ``git log --reverse`` so the list is in commit order
    (oldest first).
    """
    raw = subprocess.check_output(  # noqa: S603
        ["git", "log", "--reverse", "--format=%H%x00%s%x00%b%x1e", f"{base}..HEAD"],
        text=True,
    )
    commits: list[tuple[str, str, str]] = []
    for record in raw.split("\x1e"):
        stripped = record.strip()
        if not stripped:
            continue
        parts = stripped.split("\x00", 2)
        if len(parts) != 3:
            continue
        sha = parts[0].strip()
        subject = parts[1].strip()
        body = parts[2].strip()
        match = CHANGEID_RE.search(body)
        change_id = match.group(1) if match else ""
        commits.append((sha, subject, change_id))
    return commits


def match_commit(
    prefix: str,
    commits: list[tuple[str, str, str]],
) -> tuple[str, str, str]:
    """Match a SHA or Change-Id prefix to exactly one commit.

    Auto-detect: if prefix starts with ``I`` and the rest is hex, match
    against the change_id field; otherwise match against the sha field.

    Calls ``sys.exit(1)`` with an error message on no match or ambiguous
    match.
    """
    if is_change_id_prefix(prefix):
        matches = [c for c in commits if c[2].startswith(prefix)]
        field_name = "Change-Id"
    else:
        matches = [c for c in commits if c[0].startswith(prefix)]
        field_name = "SHA"

    if len(matches) == 0:
        console_error(f"no commit found matching {field_name} prefix '{prefix}'")
        sys.exit(ExitCode.STACK_NOT_FOUND)
    if len(matches) > 1:
        console_error(
            f"ambiguous {field_name} prefix '{prefix}' matches {len(matches)} commits:",
        )
        for sha, subject, change_id in matches:
            console.print(f"  {sha[:12]} {subject} ({change_id[:12]})", style="red")
        sys.exit(ExitCode.INVALID_STATE)

    return matches[0]


def run_scripted_rebase(
    base: str,
    script_content: str,
    commit_message: str | None = None,
) -> None:
    """Run ``git rebase -i`` with a custom sequence-editor script.

    Writes *script_content* to a temporary Python file, sets it as
    ``GIT_SEQUENCE_EDITOR``, then executes the rebase.

    If *commit_message* is provided, also writes a second temp Python
    file and sets it as ``GIT_EDITOR``; that script overwrites whatever
    file git passes it with *commit_message*. This lets callers drive a
    ``squash`` action non-interactively.

    All temp files are cleaned up regardless of outcome.
    """
    seq_fd, seq_path = tempfile.mkstemp(suffix=".py", prefix="mergify_rebase_")
    editor_path: str | None = None
    try:
        with os.fdopen(seq_fd, "w") as f:
            f.write(script_content)
        pathlib.Path(seq_path).chmod(0o755)

        env = os.environ.copy()
        python = shlex.quote(sys.executable)
        seq_quoted = shlex.quote(seq_path)
        env["GIT_SEQUENCE_EDITOR"] = f"{python} {seq_quoted}"

        if commit_message is not None:
            editor_fd, editor_path = tempfile.mkstemp(
                suffix=".py",
                prefix="mergify_editor_",
            )
            editor_script = (
                "#!/usr/bin/env python3\n"
                "import sys\n"
                "message = " + repr(commit_message) + "\n"
                "if not message.endswith('\\n'):\n"
                "    message += '\\n'\n"
                "with open(sys.argv[1], 'w') as f:\n"
                "    f.write(message)\n"
            )
            with os.fdopen(editor_fd, "w") as f:
                f.write(editor_script)
            pathlib.Path(editor_path).chmod(0o755)
            editor_quoted = shlex.quote(editor_path)
            env["GIT_EDITOR"] = f"{python} {editor_quoted}"

        result = subprocess.run(  # noqa: S603
            ["git", "rebase", "-i", base],
            env=env,
        )

        if result.returncode != 0:
            console_error("rebase failed — there may be conflicts")
            console.print(
                "Resolve conflicts then run: git rebase --continue",
            )
            console.print(
                "Or abort the rebase with: git rebase --abort",
            )
            sys.exit(ExitCode.CONFLICT)
    finally:
        for path in (seq_path, editor_path):
            if path is None:
                continue
            tmp = pathlib.Path(path)
            if tmp.exists():
                tmp.unlink()


def run_rebase(base: str, ordered_shas: list[str]) -> None:
    """Run ``git rebase -i`` reordering picks to match *ordered_shas*."""
    script_content = (
        "#!/usr/bin/env python3\n"
        "import sys\n"
        "order = " + repr(ordered_shas) + "\n"
        "todo_path = sys.argv[1]\n"
        "with open(todo_path) as f:\n"
        "    lines = f.readlines()\n"
        "pick_lines = {}\n"
        "other_lines = []\n"
        "for line in lines:\n"
        "    stripped = line.strip()\n"
        "    if stripped and not stripped.startswith('#'):\n"
        "        parts = stripped.split(None, 2)\n"
        "        if len(parts) >= 2:\n"
        "            pick_lines[parts[1]] = line\n"
        "        else:\n"
        "            other_lines.append(line)\n"
        "    else:\n"
        "        other_lines.append(line)\n"
        "reordered = []\n"
        "for sha in order:\n"
        "    for key in pick_lines:\n"
        "        if sha.startswith(key) or key.startswith(sha):\n"
        "            reordered.append(pick_lines[key])\n"
        "            break\n"
        "with open(todo_path, 'w') as f:\n"
        "    f.writelines(reordered + other_lines)\n"
    )
    run_scripted_rebase(base, script_content)


def run_action_rebase(
    base: str,
    ordered_shas: list[str],
    actions: dict[str, str],
    commit_message: str | None = None,
) -> None:
    """Run ``git rebase -i`` reordering picks and changing their action.

    *ordered_shas* is the desired full order (as in ``run_rebase``).

    *actions* maps sha -> action string (``"squash"`` or ``"fixup"``).
    Each listed sha has its ``pick`` replaced by the given action.
    Shas not in *actions* stay as ``pick``.

    *commit_message* is passed through to ``run_scripted_rebase`` and
    sets ``GIT_EDITOR`` when provided — useful when ``actions`` contains
    ``"squash"`` (which triggers git's commit-message editor).
    """
    script_content = (
        "#!/usr/bin/env python3\n"
        "import sys\n"
        "order = " + repr(ordered_shas) + "\n"
        "actions = " + repr(actions) + "\n"
        "todo_path = sys.argv[1]\n"
        "with open(todo_path) as f:\n"
        "    lines = f.readlines()\n"
        "pick_lines = {}\n"
        "other_lines = []\n"
        "for line in lines:\n"
        "    stripped = line.strip()\n"
        "    if stripped and not stripped.startswith('#'):\n"
        "        parts = stripped.split(None, 2)\n"
        "        if len(parts) >= 2:\n"
        "            pick_lines[parts[1]] = line\n"
        "        else:\n"
        "            other_lines.append(line)\n"
        "    else:\n"
        "        other_lines.append(line)\n"
        "reordered = []\n"
        "for sha in order:\n"
        "    for key, line in pick_lines.items():\n"
        "        if sha.startswith(key) or key.startswith(sha):\n"
        "            action = None\n"
        "            for act_sha, act in actions.items():\n"
        "                if sha.startswith(act_sha) or act_sha.startswith(sha):\n"
        "                    action = act\n"
        "                    break\n"
        "            if action is not None:\n"
        "                _parts = line.split(None, 1)\n"
        "                rest = _parts[1] if len(_parts) > 1 else ''\n"
        "                line = action + ' ' + rest\n"
        "            reordered.append(line)\n"
        "            break\n"
        "with open(todo_path, 'w') as f:\n"
        "    f.writelines(reordered + other_lines)\n"
    )
    run_scripted_rebase(base, script_content, commit_message=commit_message)


def display_plan(
    title: str,
    commits: list[tuple[str, str, str]],
) -> None:
    """Print the planned commit order."""
    console.log(title)
    for idx, (sha, subject, change_id) in enumerate(commits, 1):
        cid_display = f" ({change_id[:12]})" if change_id else ""
        console.log(f"  {idx}. {sha[:12]} {subject}{cid_display}")


def display_action_plan(
    title: str,
    commits: list[tuple[str, str, str]],
    actions: dict[str, str],
) -> None:
    """Print the planned commit order, tagging rows with their action."""
    console.log(title)
    for idx, (sha, subject, change_id) in enumerate(commits, 1):
        cid_display = f" ({change_id[:12]})" if change_id else ""
        tag = ""
        for act_sha, act in actions.items():
            if sha.startswith(act_sha) or act_sha.startswith(sha):
                tag = f" [{act}]"
                break
        console.log(f"  {idx}. {sha[:12]} {subject}{cid_display}{tag}")


async def stack_reorder(
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

    if len(commit_prefixes) != len(commits):
        console_error(
            f"expected {len(commits)} commits but got {len(commit_prefixes)} prefixes",
        )
        sys.exit(ExitCode.INVALID_STATE)

    # Match each prefix to a commit
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

    # Check if already in order
    current_shas = [c[0] for c in commits]
    if matched_shas == current_shas:
        console.print(
            "Stack is already in the requested order",
            style="green",
        )
        return

    display_plan("Reorder plan:", matched)

    if dry_run:
        console.print("Dry run — no changes made", style="green")
        return

    run_rebase(base, matched_shas)
    console.print("Stack reordered successfully.", style="green")
