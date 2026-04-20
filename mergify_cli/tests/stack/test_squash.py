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

from mergify_cli.stack.squash import stack_fixup


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


def _get_head_message(repo: pathlib.Path, sha: str = "HEAD") -> str:
    return _run_git("log", "-1", "--format=%B", sha, cwd=repo).strip()


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


class TestStackFixup:
    async def test_fixup_single_into_parent(
        self,
        stack_repo: tuple[pathlib.Path, list[tuple[str, str | None]]],
    ) -> None:
        """fixup B: B folds into A, A's message preserved, C unchanged."""
        repo, commits = stack_repo
        os.chdir(repo)

        sha_b = commits[1][0][:12]

        await stack_fixup([sha_b], dry_run=False)

        subjects = _get_commit_subjects(repo)
        feature_subjects = [s for s in subjects if s.startswith("Commit")]
        assert feature_subjects == ["Commit A", "Commit C"]

        # Verify both files are present (B's content was preserved)
        assert (repo / "a.txt").exists()
        assert (repo / "b.txt").exists()
        assert (repo / "c.txt").exists()
