from __future__ import annotations

import re
import subprocess
import typing

from mergify_cli.stack import setup
from mergify_cli.stack.changes import CHANGEID_RE


if typing.TYPE_CHECKING:
    import pathlib

    import pytest

    from mergify_cli.tests import utils as test_utils


async def test_setup(
    git_mock: test_utils.GitMock,
    tmp_path: pytest.TempdirFactory,
) -> None:
    hooks_dir = typing.cast("pathlib.Path", tmp_path) / ".git" / "hooks"
    hooks_dir.mkdir(parents=True)
    git_mock.mock("rev-parse", "--git-path", "hooks", output=str(hooks_dir))
    await setup.stack_setup()

    commit_msg_hook = hooks_dir / "commit-msg"
    assert commit_msg_hook.exists()

    prepare_commit_msg_hook = hooks_dir / "prepare-commit-msg"
    assert prepare_commit_msg_hook.exists()


def get_commit_message(repo_path: pathlib.Path) -> str:
    """Get the current HEAD commit message."""
    return subprocess.check_output(
        ["git", "log", "-1", "--format=%B"],
        text=True,
        cwd=repo_path,
    )


def get_change_id(message: str) -> str | None:
    """Extract Change-Id from a commit message."""
    match = CHANGEID_RE.search(message)
    return match.group(1) if match else None


def test_commit_gets_change_id(git_repo_with_hooks: pathlib.Path) -> None:
    """Test that a new commit gets a Change-Id from the commit-msg hook."""
    # Create a file and commit
    (git_repo_with_hooks / "file.txt").write_text("content")
    subprocess.run(["git", "add", "file.txt"], check=True, cwd=git_repo_with_hooks)
    subprocess.run(
        ["git", "commit", "-m", "Initial commit"],
        check=True,
        cwd=git_repo_with_hooks,
    )

    message = get_commit_message(git_repo_with_hooks)
    change_id = get_change_id(message)

    assert change_id is not None, f"Expected Change-Id in message:\n{message}"
    assert re.match(r"^I[0-9a-f]{40}$", change_id)


def test_amend_with_m_flag_preserves_change_id(
    git_repo_with_hooks: pathlib.Path,
) -> None:
    """Test that amending a commit with -m flag preserves the Change-Id.

    This is the specific scenario where tools like Claude Code amend commits
    by passing the message via -m flag, which would otherwise lose the Change-Id.
    """
    import time

    # Create initial commit with Change-Id
    (git_repo_with_hooks / "file.txt").write_text("content")
    subprocess.run(["git", "add", "file.txt"], check=True, cwd=git_repo_with_hooks)
    subprocess.run(
        ["git", "commit", "-m", "Initial commit"],
        check=True,
        cwd=git_repo_with_hooks,
    )

    original_message = get_commit_message(git_repo_with_hooks)
    original_change_id = get_change_id(original_message)
    assert original_change_id is not None

    # Wait a bit so the hook can detect this is an amend (author date will be old)
    time.sleep(2)

    # Amend with -m flag (this is what Claude Code does)
    subprocess.run(
        ["git", "commit", "--amend", "-m", "Amended commit"],
        check=True,
        cwd=git_repo_with_hooks,
    )

    amended_message = get_commit_message(git_repo_with_hooks)
    amended_change_id = get_change_id(amended_message)

    assert amended_change_id is not None, (
        f"Expected Change-Id in amended message:\n{amended_message}"
    )
    assert amended_change_id == original_change_id, (
        f"Change-Id should be preserved during amend.\n"
        f"Original: {original_change_id}\n"
        f"After amend: {amended_change_id}"
    )


def test_amend_without_m_flag_preserves_change_id(
    git_repo_with_hooks: pathlib.Path,
) -> None:
    """Test that amending without -m flag also preserves the Change-Id."""
    # Create initial commit with Change-Id
    (git_repo_with_hooks / "file.txt").write_text("content")
    subprocess.run(["git", "add", "file.txt"], check=True, cwd=git_repo_with_hooks)
    subprocess.run(
        ["git", "commit", "-m", "Initial commit"],
        check=True,
        cwd=git_repo_with_hooks,
    )

    original_message = get_commit_message(git_repo_with_hooks)
    original_change_id = get_change_id(original_message)
    assert original_change_id is not None

    # Amend without changing message
    subprocess.run(
        ["git", "commit", "--amend", "--no-edit"],
        check=True,
        cwd=git_repo_with_hooks,
    )

    amended_message = get_commit_message(git_repo_with_hooks)
    amended_change_id = get_change_id(amended_message)

    assert amended_change_id is not None
    assert amended_change_id == original_change_id


def test_new_commit_after_amend_gets_new_change_id(
    git_repo_with_hooks: pathlib.Path,
) -> None:
    """Test that a new commit (not an amend) gets a new Change-Id."""
    # Create first commit
    (git_repo_with_hooks / "file1.txt").write_text("content1")
    subprocess.run(["git", "add", "file1.txt"], check=True, cwd=git_repo_with_hooks)
    subprocess.run(
        ["git", "commit", "-m", "First commit"],
        check=True,
        cwd=git_repo_with_hooks,
    )

    first_change_id = get_change_id(get_commit_message(git_repo_with_hooks))
    assert first_change_id is not None

    # Create second commit (should get a different Change-Id)
    (git_repo_with_hooks / "file2.txt").write_text("content2")
    subprocess.run(["git", "add", "file2.txt"], check=True, cwd=git_repo_with_hooks)
    subprocess.run(
        ["git", "commit", "-m", "Second commit"],
        check=True,
        cwd=git_repo_with_hooks,
    )

    second_change_id = get_change_id(get_commit_message(git_repo_with_hooks))
    assert second_change_id is not None
    assert second_change_id != first_change_id, (
        "Each commit should have a unique Change-Id"
    )
