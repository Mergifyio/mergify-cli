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

from mergify_cli.stack.reorder import get_stack_commits
from mergify_cli.stack.reorder import match_commit
from mergify_cli.stack.reorder import stack_reorder


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


class TestGetStackCommits:
    def test_returns_commits_in_order(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        repo, expected_commits = stack_repo
        os.chdir(repo)
        base = _run_git("merge-base", "origin/main", "HEAD", cwd=repo)
        result = get_stack_commits(base)
        assert len(result) == 3
        for (sha, _subject, change_id), (expected_sha, expected_cid) in zip(
            result,
            expected_commits,
            strict=True,
        ):
            assert sha == expected_sha
            assert expected_cid is not None
            assert change_id == expected_cid


class TestMatchCommit:
    def test_match_by_sha_prefix(self) -> None:
        commits = [
            ("abc123def456", "Commit A", "I0000000000000000000000000000000000000001"),
            ("def456abc123", "Commit B", "I0000000000000000000000000000000000000002"),
        ]
        result = match_commit("abc", commits)
        assert result == commits[0]

    def test_match_by_change_id_prefix(self) -> None:
        commits = [
            ("abc123def456", "Commit A", "I1111111111111111111111111111111111111111"),
            ("def456abc123", "Commit B", "I2222222222222222222222222222222222222222"),
        ]
        result = match_commit("I111", commits)
        assert result == commits[0]

    def test_no_match_exits(self) -> None:
        commits = [
            ("abc123def456", "Commit A", "I1111111111111111111111111111111111111111"),
        ]
        with pytest.raises(SystemExit) as exc_info:
            match_commit("zzz", commits)
        assert exc_info.value.code == 1

    def test_ambiguous_match_exits(self) -> None:
        commits = [
            ("abc123000000", "Commit A", "I1111111111111111111111111111111111111111"),
            ("abc123999999", "Commit B", "I2222222222222222222222222222222222222222"),
        ]
        with pytest.raises(SystemExit) as exc_info:
            match_commit("abc123", commits)
        assert exc_info.value.code == 1


class TestStackReorder:
    async def test_reorder_success(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Reorder A,B,C -> C,A,B and verify new order."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_a = commits[0][0][:12]
        sha_b = commits[1][0][:12]
        sha_c = commits[2][0][:12]

        await stack_reorder([sha_c, sha_a, sha_b], dry_run=False)

        subjects = _get_commit_subjects(repo)
        # Filter to only our feature commits
        feature_subjects = [s for s in subjects if s.startswith("Commit")]
        assert feature_subjects == ["Commit C", "Commit A", "Commit B"]

    async def test_reorder_with_change_id(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Reorder using Change-Id prefixes."""
        repo, commits = stack_repo
        os.chdir(repo)

        cid_a = commits[0][1]
        cid_b = commits[1][1]
        cid_c = commits[2][1]
        assert cid_a is not None
        assert cid_b is not None
        assert cid_c is not None

        # Use Change-Id prefixes (first 8 chars)
        await stack_reorder(
            [cid_c[:8], cid_a[:8], cid_b[:8]],
            dry_run=False,
        )

        subjects = _get_commit_subjects(repo)
        feature_subjects = [s for s in subjects if s.startswith("Commit")]
        assert feature_subjects == ["Commit C", "Commit A", "Commit B"]

    async def test_reorder_dry_run(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Verify dry-run doesn't change anything."""
        repo, commits = stack_repo
        os.chdir(repo)

        head_before = _run_git("rev-parse", "HEAD", cwd=repo)

        sha_c = commits[2][0][:12]
        sha_a = commits[0][0][:12]
        sha_b = commits[1][0][:12]

        await stack_reorder([sha_c, sha_a, sha_b], dry_run=True)

        head_after = _run_git("rev-parse", "HEAD", cwd=repo)
        assert head_before == head_after

    async def test_reorder_already_in_order(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Pass commits in current order: no rebase should happen."""
        repo, commits = stack_repo
        os.chdir(repo)

        head_before = _run_git("rev-parse", "HEAD", cwd=repo)

        sha_a = commits[0][0][:12]
        sha_b = commits[1][0][:12]
        sha_c = commits[2][0][:12]

        await stack_reorder([sha_a, sha_b, sha_c], dry_run=False)

        head_after = _run_git("rev-parse", "HEAD", cwd=repo)
        assert head_before == head_after

    async def test_reorder_wrong_count_too_few(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Pass 2 prefixes for a 3-commit stack."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_a = commits[0][0][:12]
        sha_b = commits[1][0][:12]

        with pytest.raises(SystemExit) as exc_info:
            await stack_reorder([sha_a, sha_b], dry_run=False)
        assert exc_info.value.code == 1

    async def test_reorder_wrong_count_too_many(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Pass 4 prefixes for a 3-commit stack."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_a = commits[0][0][:12]
        sha_b = commits[1][0][:12]
        sha_c = commits[2][0][:12]

        with pytest.raises(SystemExit) as exc_info:
            await stack_reorder(
                [sha_a, sha_b, sha_c, sha_a],
                dry_run=False,
            )
        assert exc_info.value.code == 1

    async def test_reorder_unknown_prefix(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Pass a prefix that doesn't match any commit."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_a = commits[0][0][:12]
        sha_b = commits[1][0][:12]

        with pytest.raises(SystemExit) as exc_info:
            await stack_reorder(
                [sha_a, sha_b, "deadbeef1234"],
                dry_run=False,
            )
        assert exc_info.value.code == 1

    async def test_reorder_ambiguous_prefix(self) -> None:
        """Test match_commit logic with ambiguous prefix."""
        commits = [
            (
                "abc1230000000000000000000000000000000000",
                "A",
                "Ia000000000000000000000000000000000000000",
            ),
            (
                "abc1239999999999999999999999999999999999",
                "B",
                "Ib000000000000000000000000000000000000000",
            ),
            (
                "def4560000000000000000000000000000000000",
                "C",
                "Ic000000000000000000000000000000000000000",
            ),
        ]
        with pytest.raises(SystemExit) as exc_info:
            match_commit("abc123", commits)
        assert exc_info.value.code == 1

    async def test_reorder_duplicate_prefix(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Pass same prefix twice."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_a = commits[0][0][:12]
        sha_b = commits[1][0][:12]

        with pytest.raises(SystemExit) as exc_info:
            await stack_reorder([sha_a, sha_a, sha_b], dry_run=False)
        assert exc_info.value.code == 1

    async def test_reorder_empty_stack(
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
        # We need to pass at least 1 prefix to click, but stack_reorder
        # checks the stack first - empty stack returns early
        # Actually the function validates count vs stack size, and since
        # there are no commits it would say "no commits in the stack"
        # Let's just call with an empty list to test the empty branch
        await stack_reorder([], dry_run=False)
