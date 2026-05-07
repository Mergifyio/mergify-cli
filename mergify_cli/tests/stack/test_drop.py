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
from mergify_cli.stack.drop import stack_drop


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
    (repo / filename).write_text(content)
    _run_git("add", filename, cwd=repo)
    _run_git("commit", "-m", message, cwd=repo)
    sha = _run_git("rev-parse", "HEAD", cwd=repo)
    body = _run_git("log", "-1", "--format=%b", "HEAD", cwd=repo)
    change_id_match = re.search(r"Change-Id: (I[0-9a-z]{40})", body)
    return sha, change_id_match.group(1) if change_id_match else None


def _get_commit_subjects(repo: pathlib.Path, n: int = 10) -> list[str]:
    raw = _run_git(
        "log",
        "--reverse",
        f"-{n}",
        "--format=%s",
        cwd=repo,
    )
    return [line for line in raw.splitlines() if line.strip()]


def _setup_tracking(repo: pathlib.Path) -> None:
    origin_path = repo.parent / f"{repo.name}_origin.git"
    _run_git("init", "--bare", str(origin_path))
    _run_git("remote", "add", "origin", str(origin_path), cwd=repo)
    _run_git("push", "origin", "main", cwd=repo)
    _run_git("branch", "--set-upstream-to=origin/main", cwd=repo)


@pytest.fixture
def stack_repo(
    git_repo_with_hooks: pathlib.Path,
) -> tuple[pathlib.Path, list[tuple[str, str | None]]]:
    """Create a repo with 3 feature commits (A, B, C) on top of main."""
    repo = git_repo_with_hooks

    (repo / "init.txt").write_text("init")
    _run_git("add", "init.txt", cwd=repo)
    _run_git("commit", "-m", "Initial commit", cwd=repo)

    _setup_tracking(repo)

    _run_git("checkout", "-b", "feature", "main", cwd=repo)
    _run_git("branch", "--set-upstream-to=origin/main", cwd=repo)

    commits = []
    for label, filename in [("A", "a.txt"), ("B", "b.txt"), ("C", "c.txt")]:
        sha, cid = _create_commit(repo, filename, f"content {label}", f"Commit {label}")
        commits.append((sha, cid))

    return repo, commits


class TestStackDrop:
    async def test_drop_middle_commit(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """drop B: stack becomes [A, C]; B's file disappears."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_b = commits[1][0][:12]

        await stack_drop([sha_b], dry_run=False)

        feature = [s for s in _get_commit_subjects(repo) if s.startswith("Commit")]
        assert feature == ["Commit A", "Commit C"]

        assert (repo / "a.txt").exists()
        assert not (repo / "b.txt").exists()
        assert (repo / "c.txt").exists()

    async def test_drop_first_commit(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Dropping the first commit of the stack works."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_a = commits[0][0][:12]

        await stack_drop([sha_a], dry_run=False)

        feature = [s for s in _get_commit_subjects(repo) if s.startswith("Commit")]
        assert feature == ["Commit B", "Commit C"]

        assert not (repo / "a.txt").exists()
        assert (repo / "b.txt").exists()
        assert (repo / "c.txt").exists()

    async def test_drop_last_commit(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Dropping HEAD works."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_c = commits[2][0][:12]

        await stack_drop([sha_c], dry_run=False)

        feature = [s for s in _get_commit_subjects(repo) if s.startswith("Commit")]
        assert feature == ["Commit A", "Commit B"]

        assert not (repo / "c.txt").exists()

    async def test_drop_multiple(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """drop A C: only B remains."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_a = commits[0][0][:12]
        sha_c = commits[2][0][:12]

        await stack_drop([sha_a, sha_c], dry_run=False)

        feature = [s for s in _get_commit_subjects(repo) if s.startswith("Commit")]
        assert feature == ["Commit B"]

    async def test_drop_all(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Dropping every stack commit empties the stack."""
        repo, commits = stack_repo
        os.chdir(repo)

        prefixes = [c[0][:12] for c in commits]

        await stack_drop(prefixes, dry_run=False)

        feature = [s for s in _get_commit_subjects(repo) if s.startswith("Commit")]
        assert feature == []

    async def test_drop_by_change_id(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Drop by Change-Id prefix works."""
        repo, commits = stack_repo
        os.chdir(repo)

        cid_b = commits[1][1]
        assert cid_b is not None

        await stack_drop([cid_b[:8]], dry_run=False)

        feature = [s for s in _get_commit_subjects(repo) if s.startswith("Commit")]
        assert feature == ["Commit A", "Commit C"]

    async def test_drop_unknown_prefix_errors(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        repo, _commits = stack_repo
        os.chdir(repo)

        with pytest.raises(SystemExit) as exc_info:
            await stack_drop(["deadbeef"], dry_run=False)
        assert exc_info.value.code == ExitCode.STACK_NOT_FOUND

    async def test_drop_duplicate_prefix_errors(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        repo, commits = stack_repo
        os.chdir(repo)

        sha_b = commits[1][0][:12]

        with pytest.raises(SystemExit) as exc_info:
            await stack_drop([sha_b, sha_b], dry_run=False)
        assert exc_info.value.code == ExitCode.INVALID_STATE

    async def test_drop_dry_run(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        repo, commits = stack_repo
        os.chdir(repo)

        head_before = _run_git("rev-parse", "HEAD", cwd=repo)

        sha_b = commits[1][0][:12]
        await stack_drop([sha_b], dry_run=True)

        head_after = _run_git("rev-parse", "HEAD", cwd=repo)
        assert head_before == head_after

    async def test_drop_empty_stack(
        self,
        git_repo_with_hooks: pathlib.Path,
    ) -> None:
        repo = git_repo_with_hooks

        (repo / "init.txt").write_text("init")
        _run_git("add", "init.txt", cwd=repo)
        _run_git("commit", "-m", "Initial commit", cwd=repo)

        _setup_tracking(repo)

        _run_git("checkout", "-b", "feature", "main", cwd=repo)
        _run_git("branch", "--set-upstream-to=origin/main", cwd=repo)

        os.chdir(repo)

        # Should return without error — no commits to drop
        await stack_drop(["abc"], dry_run=False)
