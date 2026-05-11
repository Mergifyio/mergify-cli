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
from mergify_cli.stack.reword import stack_reword


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


class TestStackReword:
    async def test_reword_middle_commit(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """reword B with -m: B's message changes; A and C untouched."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_b = commits[1][0][:12]

        await stack_reword(sha_b, message="Renamed B", dry_run=False)

        feature = [s for s in _get_commit_subjects(repo) if not s.startswith("Initial")]
        assert feature == ["Commit A", "Renamed B", "Commit C"]

    async def test_reword_first_commit(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        repo, commits = stack_repo
        os.chdir(repo)

        sha_a = commits[0][0][:12]

        await stack_reword(sha_a, message="Renamed A", dry_run=False)

        feature = [s for s in _get_commit_subjects(repo) if not s.startswith("Initial")]
        assert feature == ["Renamed A", "Commit B", "Commit C"]

    async def test_reword_last_commit(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        repo, commits = stack_repo
        os.chdir(repo)

        sha_c = commits[2][0][:12]

        await stack_reword(sha_c, message="Renamed C", dry_run=False)

        feature = [s for s in _get_commit_subjects(repo) if not s.startswith("Initial")]
        assert feature == ["Commit A", "Commit B", "Renamed C"]

    async def test_reword_by_change_id(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        repo, commits = stack_repo
        os.chdir(repo)

        cid_b = commits[1][1]
        assert cid_b is not None

        await stack_reword(cid_b[:8], message="Via Change-Id", dry_run=False)

        feature = [s for s in _get_commit_subjects(repo) if not s.startswith("Initial")]
        assert feature == ["Commit A", "Via Change-Id", "Commit C"]

    async def test_reword_multiline_message(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """Multi-line message is preserved (passed via temp file, not -m)."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_b = commits[1][0][:12]
        new_msg = "Renamed B\n\nThis paragraph explains what B does.\n\nFixes #42"

        await stack_reword(sha_b, message=new_msg, dry_run=False)

        log = _run_git("log", "--format=%H %s", "main..HEAD", cwd=repo)
        new_sha = next(
            line.split(" ", 1)[0] for line in log.splitlines() if "Renamed B" in line
        )
        body = _run_git("log", "-1", "--format=%B", new_sha, cwd=repo).strip()
        assert "Renamed B" in body
        assert "This paragraph explains what B does." in body
        assert "Fixes #42" in body

    async def test_reword_unknown_prefix_errors(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        repo, _commits = stack_repo
        os.chdir(repo)

        with pytest.raises(SystemExit) as exc_info:
            await stack_reword("deadbeef", message="X", dry_run=False)
        assert exc_info.value.code == ExitCode.STACK_NOT_FOUND

    async def test_reword_dry_run(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        repo, commits = stack_repo
        os.chdir(repo)

        head_before = _run_git("rev-parse", "HEAD", cwd=repo)

        sha_b = commits[1][0][:12]
        await stack_reword(sha_b, message="X", dry_run=True)

        head_after = _run_git("rev-parse", "HEAD", cwd=repo)
        assert head_before == head_after

    async def test_reword_empty_stack(
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

        # Should return without error — no commits to reword
        await stack_reword("abc", message="X", dry_run=False)
