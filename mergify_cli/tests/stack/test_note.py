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
import re
import subprocess
import sys
from typing import TYPE_CHECKING

import pytest

from mergify_cli.stack.note import NOTES_REF
from mergify_cli.stack.note import stack_note


if TYPE_CHECKING:
    import pathlib


def _run_git(*args: str, cwd: pathlib.Path | None = None) -> str:
    return subprocess.check_output(
        ["git", *args],
        text=True,
        cwd=cwd,
    ).strip()


@pytest.fixture
def repo_with_commit(git_repo_with_hooks: pathlib.Path) -> pathlib.Path:
    """A repo with one commit on main, cwd set to the repo."""
    (git_repo_with_hooks / "file.txt").write_text("hello")
    _run_git("add", "file.txt", cwd=git_repo_with_hooks)
    _run_git("commit", "-m", "First commit", cwd=git_repo_with_hooks)
    os.chdir(git_repo_with_hooks)
    return git_repo_with_hooks


class TestStackNote:
    async def test_add_attaches_note_to_head(
        self,
        repo_with_commit: pathlib.Path,
    ) -> None:
        """`stack note -m MSG` attaches the note to HEAD in refs/notes/mergify."""
        await stack_note(
            commit=None,
            message="fixed a typo",
            append=False,
            remove=False,
        )

        note_text = _run_git(
            "notes",
            f"--ref={NOTES_REF}",
            "show",
            "HEAD",
            cwd=repo_with_commit,
        )
        assert note_text == "fixed a typo"

    async def test_add_to_specific_sha(
        self,
        repo_with_commit: pathlib.Path,
    ) -> None:
        """`stack note SHA -m MSG` attaches the note to that SHA."""
        # Make a second commit so HEAD differs from the target
        (repo_with_commit / "file2.txt").write_text("world")
        _run_git("add", "file2.txt", cwd=repo_with_commit)
        _run_git("commit", "-m", "Second commit", cwd=repo_with_commit)
        target_sha = _run_git("rev-parse", "HEAD~1", cwd=repo_with_commit)

        await stack_note(
            commit=target_sha[:10],
            message="note for first",
            append=False,
            remove=False,
        )

        note_text = _run_git(
            "notes",
            f"--ref={NOTES_REF}",
            "show",
            target_sha,
            cwd=repo_with_commit,
        )
        assert note_text == "note for first"

    async def test_add_by_change_id_prefix(
        self,
        repo_with_commit: pathlib.Path,
    ) -> None:
        """`stack note <change-id-prefix>` resolves against stack commits."""
        # Set upstream so get_trunk() works
        origin_path = repo_with_commit.parent / f"{repo_with_commit.name}_origin.git"
        _run_git("init", "--bare", str(origin_path))
        _run_git("remote", "add", "origin", str(origin_path), cwd=repo_with_commit)
        _run_git("push", "origin", "main", cwd=repo_with_commit)
        _run_git("branch", "--set-upstream-to=origin/main", cwd=repo_with_commit)
        _run_git("checkout", "-b", "feature", cwd=repo_with_commit)
        _run_git("branch", "--set-upstream-to=origin/main", cwd=repo_with_commit)

        # Hook adds Change-Id automatically
        (repo_with_commit / "feat.txt").write_text("feat")
        _run_git("add", "feat.txt", cwd=repo_with_commit)
        _run_git("commit", "-m", "Feature commit", cwd=repo_with_commit)

        body = _run_git("log", "-1", "--format=%b", cwd=repo_with_commit)
        m = re.search(r"Change-Id: (I[0-9a-f]{40})", body)
        assert m is not None, (
            "commit-msg hook did not inject a Change-Id — "
            "check git_repo_with_hooks fixture"
        )
        change_id = m.group(1)
        target_sha = _run_git("rev-parse", "HEAD", cwd=repo_with_commit)

        await stack_note(
            commit=change_id[:9],
            message="by change-id",
            append=False,
            remove=False,
        )

        note_text = _run_git(
            "notes",
            f"--ref={NOTES_REF}",
            "show",
            target_sha,
            cwd=repo_with_commit,
        )
        assert note_text == "by change-id"

    async def test_append_concatenates(
        self,
        repo_with_commit: pathlib.Path,
    ) -> None:
        """Second call with --append joins to the existing note with a blank line."""
        await stack_note(commit=None, message="first line", append=False, remove=False)
        await stack_note(commit=None, message="second line", append=True, remove=False)

        note_text = _run_git(
            "notes",
            f"--ref={NOTES_REF}",
            "show",
            "HEAD",
            cwd=repo_with_commit,
        )
        assert note_text == "first line\n\nsecond line"

    async def test_replace_is_default(
        self,
        repo_with_commit: pathlib.Path,
    ) -> None:
        """Without --append, a second call replaces the existing note."""
        await stack_note(commit=None, message="first", append=False, remove=False)
        await stack_note(commit=None, message="second", append=False, remove=False)

        note_text = _run_git(
            "notes",
            f"--ref={NOTES_REF}",
            "show",
            "HEAD",
            cwd=repo_with_commit,
        )
        assert note_text == "second"

    async def test_remove_deletes_note(
        self,
        repo_with_commit: pathlib.Path,
    ) -> None:
        """--remove deletes an existing note."""
        await stack_note(commit=None, message="doomed", append=False, remove=False)
        await stack_note(commit=None, message=None, append=False, remove=True)

        # git notes show should exit non-zero when there is no note
        result = subprocess.run(
            ["git", "notes", f"--ref={NOTES_REF}", "show", "HEAD"],
            cwd=repo_with_commit,
            check=False,
            capture_output=True,
            text=True,
        )
        assert result.returncode != 0

    async def test_remove_is_idempotent(
        self,
        repo_with_commit: pathlib.Path,  # noqa: ARG002
    ) -> None:
        """--remove on a commit without a note exits 0 and prints a message."""
        # Must not raise
        await stack_note(commit=None, message=None, append=False, remove=True)

    @pytest.mark.skipif(sys.platform == "win32", reason="fake editor uses .sh script")
    async def test_editor_fallback_strips_comments(
        self,
        repo_with_commit: pathlib.Path,
        monkeypatch: pytest.MonkeyPatch,
    ) -> None:
        """With no -m, opens $GIT_EDITOR with a template; comment lines are stripped."""
        # Fake editor: replaces the file content with a user-provided message
        # plus a comment line, simulating a user who wrote something and saved.
        editor_script = repo_with_commit / "fake-editor.sh"
        editor_script.write_text(
            "#!/bin/sh\n"
            'printf "real note text\\n# Why was this commit amended? Lines starting with # are ignored.\\n" > "$1"\n',
        )
        editor_script.chmod(0o755)
        monkeypatch.setenv("GIT_EDITOR", str(editor_script))

        await stack_note(commit=None, message=None, append=False, remove=False)

        note_text = _run_git(
            "notes",
            f"--ref={NOTES_REF}",
            "show",
            "HEAD",
            cwd=repo_with_commit,
        )
        assert note_text == "real note text"

    @pytest.mark.skipif(sys.platform == "win32", reason="fake editor uses .sh script")
    async def test_empty_message_rejected(
        self,
        repo_with_commit: pathlib.Path,
        monkeypatch: pytest.MonkeyPatch,
    ) -> None:
        """Editor returns only comment lines → sys.exit(1), no git call."""
        from mergify_cli.exit_codes import ExitCode

        editor_script = repo_with_commit / "empty-editor.sh"
        editor_script.write_text(
            '#!/bin/sh\nprintf "# only a comment\\n   \\n" > "$1"\n',
        )
        editor_script.chmod(0o755)
        monkeypatch.setenv("GIT_EDITOR", str(editor_script))

        with pytest.raises(SystemExit) as exc_info:
            await stack_note(commit=None, message=None, append=False, remove=False)
        assert exc_info.value.code == ExitCode.INVALID_STATE

        # No note should have been written
        result = subprocess.run(
            ["git", "notes", f"--ref={NOTES_REF}", "show", "HEAD"],
            cwd=repo_with_commit,
            check=False,
            capture_output=True,
        )
        assert result.returncode != 0

    async def test_empty_inline_message_rejected(
        self,
        repo_with_commit: pathlib.Path,  # noqa: ARG002
    ) -> None:
        """Explicit empty -m is rejected too."""
        from mergify_cli.exit_codes import ExitCode

        with pytest.raises(SystemExit) as exc_info:
            await stack_note(commit=None, message="   ", append=False, remove=False)
        assert exc_info.value.code == ExitCode.INVALID_STATE

    def test_cli_help_lists_note(self) -> None:
        """`mergify stack note --help` lists the command."""
        from click.testing import CliRunner

        from mergify_cli.cli import cli

        runner = CliRunner()
        result = runner.invoke(cli, ["stack", "note", "--help"])
        assert result.exit_code == 0
        assert "note" in result.output.lower()

    def test_cli_rejects_remove_with_message(self) -> None:
        """`stack note --remove -m ...` fails with a clear usage error."""
        from click.testing import CliRunner

        from mergify_cli.cli import cli

        runner = CliRunner()
        result = runner.invoke(cli, ["stack", "note", "--remove", "-m", "x"])
        assert result.exit_code != 0
        assert "--remove cannot be combined" in result.output
