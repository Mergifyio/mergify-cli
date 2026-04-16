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

from mergify_cli.exit_codes import ExitCode
from mergify_cli.stack.move import stack_move


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


def _get_commit_subjects(repo: pathlib.Path, n: int = 10) -> list[str]:
    """Return the last n commit subjects, oldest first."""
    raw = _run_git(
        "log",
        "--reverse",
        f"-{n}",
        "--format=%s",
        cwd=repo,
    )
    return [line for line in raw.splitlines() if line.strip()]


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


class TestStackMove:
    async def test_move_before(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Move C before A, verify order: C, A, B."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_a = commits[0][0][:12]
        sha_c = commits[2][0][:12]

        await stack_move(sha_c, "before", sha_a, dry_run=False)

        subjects = _get_commit_subjects(repo)
        feature_subjects = [s for s in subjects if s.startswith("Commit")]
        assert feature_subjects == ["Commit C", "Commit A", "Commit B"]

    async def test_move_after(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Move A after B, verify order: B, A, C."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_a = commits[0][0][:12]
        sha_b = commits[1][0][:12]

        await stack_move(sha_a, "after", sha_b, dry_run=False)

        subjects = _get_commit_subjects(repo)
        feature_subjects = [s for s in subjects if s.startswith("Commit")]
        assert feature_subjects == ["Commit B", "Commit A", "Commit C"]

    async def test_move_first(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Move C first, verify order: C, A, B."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_c = commits[2][0][:12]

        await stack_move(sha_c, "first", None, dry_run=False)

        subjects = _get_commit_subjects(repo)
        feature_subjects = [s for s in subjects if s.startswith("Commit")]
        assert feature_subjects == ["Commit C", "Commit A", "Commit B"]

    async def test_move_last(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Move A last, verify order: B, C, A."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_a = commits[0][0][:12]

        await stack_move(sha_a, "last", None, dry_run=False)

        subjects = _get_commit_subjects(repo)
        feature_subjects = [s for s in subjects if s.startswith("Commit")]
        assert feature_subjects == ["Commit B", "Commit C", "Commit A"]

    async def test_move_dry_run(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Verify dry-run doesn't change anything."""
        repo, commits = stack_repo
        os.chdir(repo)

        head_before = _run_git("rev-parse", "HEAD", cwd=repo)

        sha_c = commits[2][0][:12]

        await stack_move(sha_c, "first", None, dry_run=True)

        head_after = _run_git("rev-parse", "HEAD", cwd=repo)
        assert head_before == head_after

    async def test_move_already_in_position(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Move A first when A is already first, verify no-op."""
        repo, commits = stack_repo
        os.chdir(repo)

        head_before = _run_git("rev-parse", "HEAD", cwd=repo)

        sha_a = commits[0][0][:12]

        await stack_move(sha_a, "first", None, dry_run=False)

        head_after = _run_git("rev-parse", "HEAD", cwd=repo)
        assert head_before == head_after

    async def test_move_already_last(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Move C last when C is already last, verify no-op."""
        repo, commits = stack_repo
        os.chdir(repo)

        head_before = _run_git("rev-parse", "HEAD", cwd=repo)

        sha_c = commits[2][0][:12]

        await stack_move(sha_c, "last", None, dry_run=False)

        head_after = _run_git("rev-parse", "HEAD", cwd=repo)
        assert head_before == head_after

    async def test_move_commit_equals_target(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """move X before X should fail."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_a = commits[0][0][:12]

        with pytest.raises(SystemExit) as exc_info:
            await stack_move(sha_a, "before", sha_a, dry_run=False)
        assert exc_info.value.code == ExitCode.INVALID_STATE

    async def test_move_target_missing_for_before(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Call with position='before' but target=None should fail."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_a = commits[0][0][:12]

        with pytest.raises(SystemExit) as exc_info:
            await stack_move(sha_a, "before", None, dry_run=False)
        assert exc_info.value.code == ExitCode.INVALID_STATE

    async def test_move_target_missing_for_after(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Call with position='after' but target=None should fail."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_a = commits[0][0][:12]

        with pytest.raises(SystemExit) as exc_info:
            await stack_move(sha_a, "after", None, dry_run=False)
        assert exc_info.value.code == ExitCode.INVALID_STATE

    async def test_move_target_provided_for_first(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Call with position='first' and a target should fail."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_a = commits[0][0][:12]
        sha_b = commits[1][0][:12]

        with pytest.raises(SystemExit) as exc_info:
            await stack_move(sha_a, "first", sha_b, dry_run=False)
        assert exc_info.value.code == ExitCode.INVALID_STATE

    async def test_move_target_provided_for_last(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Call with position='last' and a target should fail."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_a = commits[0][0][:12]
        sha_b = commits[1][0][:12]

        with pytest.raises(SystemExit) as exc_info:
            await stack_move(sha_a, "last", sha_b, dry_run=False)
        assert exc_info.value.code == ExitCode.INVALID_STATE

    async def test_move_with_change_id(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Use Change-Id prefix, verify it works."""
        repo, commits = stack_repo
        os.chdir(repo)

        cid_c = commits[2][1]
        assert cid_c is not None

        await stack_move(cid_c[:8], "first", None, dry_run=False)

        subjects = _get_commit_subjects(repo)
        feature_subjects = [s for s in subjects if s.startswith("Commit")]
        assert feature_subjects == ["Commit C", "Commit A", "Commit B"]

    async def test_move_empty_stack(
        self,
        git_repo_with_hooks: pathlib.Path,
    ) -> None:
        """No commits between base and HEAD."""
        repo = git_repo_with_hooks

        # Create just an initial commit on main
        (repo / "init.txt").write_text("init")
        _run_git("add", "init.txt", cwd=repo)
        _run_git("commit", "-m", "Initial commit", cwd=repo)

        _setup_tracking(repo)

        # Create feature branch with no new commits
        _run_git("checkout", "-b", "feature", "main", cwd=repo)
        _run_git("branch", "--set-upstream-to=origin/main", cwd=repo)

        os.chdir(repo)

        # Should just print no-op and return without error
        await stack_move("anything", "first", None, dry_run=False)
