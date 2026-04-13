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
from typing import TYPE_CHECKING

import pytest

from mergify_cli.stack.edit import stack_edit


if TYPE_CHECKING:
    import pathlib


def _run_git(*args: str, cwd: pathlib.Path | None = None) -> str:
    return subprocess.check_output(
        ["git", *args],
        text=True,
        cwd=cwd,
    ).strip()


def _create_commit(
    repo: pathlib.Path,
    filename: str,
    content: str,
    message: str,
) -> tuple[str, str | None]:
    """Create a commit and return (sha, change_id)."""
    (repo / filename).write_text(content)
    _run_git("add", filename, cwd=repo)
    _run_git("commit", "-m", message, cwd=repo)
    sha = _run_git("rev-parse", "HEAD", cwd=repo)
    body = _run_git("log", "-1", "--format=%b", "HEAD", cwd=repo)
    change_id_match = re.search(r"Change-Id: (I[0-9a-z]{40})", body)
    return sha, change_id_match.group(1) if change_id_match else None


def _setup_tracking(repo: pathlib.Path) -> None:
    """Create a bare origin and set up tracking for the current branch."""
    origin_path = repo.parent / f"{repo.name}_origin.git"
    _run_git("init", "--bare", str(origin_path))
    _run_git("remote", "add", "origin", str(origin_path), cwd=repo)
    _run_git("push", "origin", "main", cwd=repo)
    _run_git("branch", "--set-upstream-to=origin/main", cwd=repo)


@pytest.fixture
def stack_repo(
    git_repo_with_hooks: pathlib.Path,
) -> tuple[pathlib.Path, list[tuple[str, str | None]]]:
    """Create a repo with 3 commits (A, B, C) on a feature branch."""
    repo = git_repo_with_hooks

    # Create an initial commit on main
    (repo / "init.txt").write_text("init")
    _run_git("add", "init.txt", cwd=repo)
    _run_git("commit", "-m", "Initial commit", cwd=repo)

    _setup_tracking(repo)

    # Create a feature branch
    _run_git("checkout", "-b", "feature", "main", cwd=repo)
    _run_git("branch", "--set-upstream-to=origin/main", cwd=repo)

    # Create 3 commits
    commits = []
    for label, filename in [("A", "a.txt"), ("B", "b.txt"), ("C", "c.txt")]:
        sha, cid = _create_commit(repo, filename, f"content {label}", f"Commit {label}")
        commits.append((sha, cid))

    return repo, commits


class TestStackEdit:
    async def test_edit_stops_at_target_commit(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Editing a mid-stack commit stops the rebase at that commit."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_b = commits[1][0][:12]
        await stack_edit(commit_prefix=sha_b)

        # Rebase should have stopped — HEAD is now the target commit
        head_subject = _run_git("log", "-1", "--format=%s", cwd=repo)
        assert head_subject == "Commit B"

        # Verify we're mid-rebase
        rebase_dir = repo / ".git" / "rebase-merge"
        assert rebase_dir.exists()

        # Clean up the rebase
        _run_git("rebase", "--abort", cwd=repo)

    async def test_edit_stops_at_first_commit(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Editing the first commit in the stack works."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_a = commits[0][0][:12]
        await stack_edit(commit_prefix=sha_a)

        head_subject = _run_git("log", "-1", "--format=%s", cwd=repo)
        assert head_subject == "Commit A"

        rebase_dir = repo / ".git" / "rebase-merge"
        assert rebase_dir.exists()

        _run_git("rebase", "--abort", cwd=repo)

    async def test_edit_stops_at_last_commit(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Editing the last (HEAD) commit in the stack works."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_c = commits[2][0][:12]
        await stack_edit(commit_prefix=sha_c)

        head_subject = _run_git("log", "-1", "--format=%s", cwd=repo)
        assert head_subject == "Commit C"

        rebase_dir = repo / ".git" / "rebase-merge"
        assert rebase_dir.exists()

        _run_git("rebase", "--abort", cwd=repo)

    async def test_edit_by_change_id(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Editing by Change-Id prefix works."""
        repo, commits = stack_repo
        os.chdir(repo)

        cid_b = commits[1][1]
        assert cid_b is not None

        await stack_edit(commit_prefix=cid_b[:8])

        head_subject = _run_git("log", "-1", "--format=%s", cwd=repo)
        assert head_subject == "Commit B"

        _run_git("rebase", "--abort", cwd=repo)

    async def test_edit_unknown_prefix_exits(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Unknown commit prefix causes exit."""
        repo, _commits = stack_repo
        os.chdir(repo)

        with pytest.raises(SystemExit) as exc_info:
            await stack_edit(commit_prefix="deadbeef1234")
        assert exc_info.value.code == 1

    async def test_edit_empty_stack(
        self,
        git_repo_with_hooks: pathlib.Path,
    ) -> None:
        """Empty stack prints message and returns."""
        repo = git_repo_with_hooks

        (repo / "init.txt").write_text("init")
        _run_git("add", "init.txt", cwd=repo)
        _run_git("commit", "-m", "Initial commit", cwd=repo)

        _setup_tracking(repo)

        _run_git("checkout", "-b", "feature", "main", cwd=repo)
        _run_git("branch", "--set-upstream-to=origin/main", cwd=repo)

        os.chdir(repo)

        # Should return without error — no commits to edit
        await stack_edit(commit_prefix="abc")
