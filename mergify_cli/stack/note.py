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
from mergify_cli.stack.changes import is_change_id_prefix
from mergify_cli.stack.reorder import get_stack_commits
from mergify_cli.stack.reorder import match_commit


NOTES_REF = "refs/notes/mergify/stack"

_EDITOR_TEMPLATE = (
    "\n# Why was this commit amended? Lines starting with # are ignored.\n"
)


def _read_note_from_editor() -> str:
    """Open $GIT_EDITOR with a template, return the cleaned message."""
    editor = (
        os.environ.get("GIT_EDITOR")
        or os.environ.get("VISUAL")
        or os.environ.get("EDITOR")
        or "vi"
    )
    with tempfile.NamedTemporaryFile(
        "w",
        suffix=".txt",
        prefix="mergify_note_",
        delete=False,
        encoding="utf-8",
    ) as f:
        f.write(_EDITOR_TEMPLATE)
        path = pathlib.Path(f.name)
    try:
        editor_parts = shlex.split(editor)
        result = subprocess.run([*editor_parts, str(path)], check=False)  # noqa: S603
        if result.returncode != 0:
            msg = f"editor {editor!r} exited with status {result.returncode}"
            raise RuntimeError(msg)
        raw = path.read_text(encoding="utf-8")
    finally:
        path.unlink(missing_ok=True)
    cleaned = "\n".join(
        line for line in raw.splitlines() if not line.lstrip().startswith("#")
    )
    return cleaned.strip()


async def _resolve_commit(commit: str | None) -> tuple[str, str]:
    """Resolve *commit* (None, SHA, or Change-Id prefix) to (full_sha, subject)."""
    if commit is None:
        sha = await utils.git("rev-parse", "--verify", "HEAD^{commit}")  # noqa: RUF027
        subject = await utils.git("log", "-1", "--format=%s", sha)
        return sha, subject

    if is_change_id_prefix(commit):
        trunk = await utils.get_trunk()
        base = await utils.git("merge-base", trunk, "HEAD")
        commits = get_stack_commits(base)
        sha, subject, _ = match_commit(commit, commits)
        return sha, subject

    sha = await utils.git("rev-parse", "--verify", f"{commit}^{{commit}}")
    subject = await utils.git("log", "-1", "--format=%s", sha)
    return sha, subject


async def stack_note(
    *,
    commit: str | None,
    message: str | None,
    append: bool,
    remove: bool,
) -> None:
    os.chdir(await utils.git("rev-parse", "--show-toplevel"))
    sha, subject = await _resolve_commit(commit)

    if remove:
        try:
            await utils.git("notes", f"--ref={NOTES_REF}", "show", sha)
        except utils.CommandError:
            console.print(f"No note on {sha[:12]} {subject}.")
            return
        await utils.git("notes", f"--ref={NOTES_REF}", "remove", sha)
        console.print(f"Note removed from {sha[:12]} {subject}.")
        return

    if message is None:
        message = _read_note_from_editor()

    if not message or not message.strip():
        console_error("note is empty, nothing attached.")
        sys.exit(ExitCode.INVALID_STATE)
    message = message.strip()

    if append:
        await utils.git(
            "notes",
            f"--ref={NOTES_REF}",
            "append",
            "-m",
            message,
            sha,
        )
    else:
        await utils.git(
            "notes",
            f"--ref={NOTES_REF}",
            "add",
            "-f",
            "-m",
            message,
            sha,
        )
    console.print(f"Note attached to {sha[:12]} {subject}.")
