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
import subprocess
import typing

from mergify_cli.stack.changes import CLAUDE_SESSION_ID_RE


if typing.TYPE_CHECKING:
    import pathlib


def test_claude_session_id_regex() -> None:
    """Test session ID extraction regex."""
    message = "Some commit\n\nChange-Id: I123abc\nClaude-Session-Id: abc-123-def"
    match = CLAUDE_SESSION_ID_RE.search(message)
    assert match is not None
    assert match.group(1) == "abc-123-def"


def test_claude_session_id_regex_with_spaces() -> None:
    """Test regex handles whitespace after colon."""
    message = "Some commit\n\nClaude-Session-Id:   session-with-spaces"
    match = CLAUDE_SESSION_ID_RE.search(message)
    assert match is not None
    assert match.group(1) == "session-with-spaces"


def test_claude_session_id_regex_no_match() -> None:
    """Test regex returns None when no session ID."""
    message = "Some commit\n\nChange-Id: I123abc"
    match = CLAUDE_SESSION_ID_RE.search(message)
    assert match is None


def get_commit_message(repo_path: pathlib.Path) -> str:
    """Get the current HEAD commit message."""
    return subprocess.check_output(
        ["git", "log", "-1", "--format=%B"],
        text=True,
        cwd=repo_path,
    )


def get_claude_session_id(message: str) -> str | None:
    """Extract Claude-Session-Id from a commit message."""
    match = CLAUDE_SESSION_ID_RE.search(message)
    return match.group(1) if match else None


def test_commit_with_session_id_env_var(
    git_repo_with_hooks: pathlib.Path,
) -> None:
    """Test that a commit gets a Claude-Session-Id when env var is set."""
    # Create a file and commit with CLAUDE_SESSION_ID set
    (git_repo_with_hooks / "file.txt").write_text("content")
    subprocess.run(["git", "add", "file.txt"], check=True, cwd=git_repo_with_hooks)

    env = os.environ.copy()
    env["CLAUDE_SESSION_ID"] = "test-session-123"
    subprocess.run(
        ["git", "commit", "-m", "Commit with session ID"],
        check=True,
        cwd=git_repo_with_hooks,
        env=env,
    )

    message = get_commit_message(git_repo_with_hooks)
    session_id = get_claude_session_id(message)

    assert session_id == "test-session-123", (
        f"Expected session ID in message:\n{message}"
    )


def test_commit_without_session_id_env_var(
    git_repo_with_hooks: pathlib.Path,
) -> None:
    """Test that a commit does not get a Claude-Session-Id when env var is not set."""
    # Create a file and commit without CLAUDE_SESSION_ID set
    (git_repo_with_hooks / "file.txt").write_text("content")
    subprocess.run(["git", "add", "file.txt"], check=True, cwd=git_repo_with_hooks)

    # Ensure CLAUDE_SESSION_ID is not set
    env = os.environ.copy()
    env.pop("CLAUDE_SESSION_ID", None)
    subprocess.run(
        ["git", "commit", "-m", "Commit without session ID"],
        check=True,
        cwd=git_repo_with_hooks,
        env=env,
    )

    message = get_commit_message(git_repo_with_hooks)
    session_id = get_claude_session_id(message)

    assert session_id is None, f"Did not expect session ID in message:\n{message}"


def test_amend_with_m_flag_preserves_session_id(
    git_repo_with_hooks: pathlib.Path,
) -> None:
    """Test that amending a commit with -m flag preserves the Claude-Session-Id."""
    import time

    # Create initial commit with Claude-Session-Id
    (git_repo_with_hooks / "file.txt").write_text("content")
    subprocess.run(["git", "add", "file.txt"], check=True, cwd=git_repo_with_hooks)

    env = os.environ.copy()
    env["CLAUDE_SESSION_ID"] = "original-session-456"
    subprocess.run(
        ["git", "commit", "-m", "Initial commit"],
        check=True,
        cwd=git_repo_with_hooks,
        env=env,
    )

    original_message = get_commit_message(git_repo_with_hooks)
    original_session_id = get_claude_session_id(original_message)
    assert original_session_id == "original-session-456"

    # Wait a bit so the hook can detect this is an amend
    time.sleep(2)

    # Amend with -m flag (without CLAUDE_SESSION_ID env var)
    env_no_session = os.environ.copy()
    env_no_session.pop("CLAUDE_SESSION_ID", None)
    subprocess.run(
        ["git", "commit", "--amend", "-m", "Amended commit"],
        check=True,
        cwd=git_repo_with_hooks,
        env=env_no_session,
    )

    amended_message = get_commit_message(git_repo_with_hooks)
    amended_session_id = get_claude_session_id(amended_message)

    assert amended_session_id == original_session_id, (
        f"Claude-Session-Id should be preserved during amend.\n"
        f"Original: {original_session_id}\n"
        f"After amend: {amended_session_id}"
    )
