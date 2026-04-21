from __future__ import annotations

import re
import subprocess
import typing

from click.testing import CliRunner

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
    git_mock.mock(
        "config",
        "--local",
        "--get-all",
        "notes.displayRef",
        output="refs/notes/mergify/*",
    )
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


async def test_get_hooks_status_no_hooks(
    git_mock: test_utils.GitMock,
    tmp_path: pathlib.Path,
) -> None:
    """Test get_hooks_status when no hooks are installed."""
    hooks_dir = tmp_path / ".git" / "hooks"
    hooks_dir.mkdir(parents=True)
    git_mock.mock("rev-parse", "--git-path", "hooks", output=str(hooks_dir))

    status = await setup.get_hooks_status()

    # Should have git_hooks section
    assert "git_hooks" in status

    git_hooks = status["git_hooks"]
    assert "commit-msg" in git_hooks
    assert "prepare-commit-msg" in git_hooks
    assert "pre-push" in git_hooks

    # All git hooks should show as not installed
    for info in git_hooks.values():
        assert info["wrapper_status"] == setup.WrapperStatus.MISSING
        assert info["script_installed"] is False


async def test_get_hooks_status_installed_hooks(
    git_mock: test_utils.GitMock,
    tmp_path: pathlib.Path,
) -> None:
    """Test get_hooks_status when hooks are installed."""
    import importlib.resources
    import shutil

    hooks_dir = tmp_path / ".git" / "hooks"
    hooks_dir.mkdir(parents=True)
    managed_dir = hooks_dir / "mergify-hooks"
    managed_dir.mkdir(parents=True)

    git_mock.mock("rev-parse", "--git-path", "hooks", output=str(hooks_dir))

    # Install hooks
    for hook_name in setup._get_git_hook_names():
        # Install wrapper
        wrapper_source = str(
            importlib.resources.files("mergify_cli.stack").joinpath(
                f"hooks/wrappers/{hook_name}",
            ),
        )
        shutil.copy(wrapper_source, hooks_dir / hook_name)

        # Install script
        script_source = str(
            importlib.resources.files("mergify_cli.stack").joinpath(
                f"hooks/scripts/{hook_name}.sh",
            ),
        )
        shutil.copy(script_source, managed_dir / f"{hook_name}.sh")

    status = await setup.get_hooks_status()

    # All git hooks should show as installed and up to date
    git_hooks = status["git_hooks"]
    for info in git_hooks.values():
        assert info["wrapper_status"] == setup.WrapperStatus.INSTALLED
        assert info["script_installed"] is True
        assert info["script_needs_update"] is False


async def test_get_hooks_status_outdated_script(
    git_mock: test_utils.GitMock,
    tmp_path: pathlib.Path,
) -> None:
    """Test get_hooks_status when script is outdated."""
    import importlib.resources
    import shutil

    hooks_dir = tmp_path / ".git" / "hooks"
    hooks_dir.mkdir(parents=True)
    managed_dir = hooks_dir / "mergify-hooks"
    managed_dir.mkdir(parents=True)

    git_mock.mock("rev-parse", "--git-path", "hooks", output=str(hooks_dir))

    # Install wrapper
    wrapper_source = str(
        importlib.resources.files("mergify_cli.stack").joinpath(
            "hooks/wrappers/commit-msg",
        ),
    )
    shutil.copy(wrapper_source, hooks_dir / "commit-msg")

    # Install script with different (old) content
    script_path = managed_dir / "commit-msg.sh"
    script_path.write_text("#!/bin/sh\n# old script content\n")

    status = await setup.get_hooks_status()

    git_hooks = status["git_hooks"]
    assert git_hooks["commit-msg"]["wrapper_status"] == setup.WrapperStatus.INSTALLED
    assert git_hooks["commit-msg"]["script_installed"] is True
    assert git_hooks["commit-msg"]["script_needs_update"] is True


def test_hooks_command_shows_status(
    git_repo_with_hooks: pathlib.Path,
) -> None:
    """Test that 'stack hooks' command shows status."""
    import os

    from mergify_cli.cli import cli

    os.chdir(git_repo_with_hooks)

    runner = CliRunner()
    result = runner.invoke(cli, ["stack", "hooks"])

    assert result.exit_code == 0
    # Git hooks section
    assert "Git Hooks Status:" in result.output
    assert "commit-msg:" in result.output
    assert "prepare-commit-msg:" in result.output
    assert "Wrapper:" in result.output


def test_hooks_command_setup_flag(
    tmp_path: pathlib.Path,
) -> None:
    """Test that 'stack hooks --setup' installs hooks."""
    import os

    from mergify_cli.cli import cli

    # Create a git repo without hooks
    subprocess.run(
        ["git", "init", "--initial-branch=main"],
        check=True,
        cwd=tmp_path,
    )
    subprocess.run(
        ["git", "config", "user.email", "test@example.com"],
        check=True,
        cwd=tmp_path,
    )
    subprocess.run(
        ["git", "config", "user.name", "Test User"],
        check=True,
        cwd=tmp_path,
    )

    os.chdir(tmp_path)

    runner = CliRunner()
    result = runner.invoke(cli, ["stack", "hooks", "--setup"])

    assert result.exit_code == 0

    # Verify hooks were installed
    hooks_dir = tmp_path / ".git" / "hooks"
    assert (hooks_dir / "commit-msg").exists()
    assert (hooks_dir / "prepare-commit-msg").exists()
    assert (hooks_dir / "post-commit").exists()
    assert (hooks_dir / "mergify-hooks" / "commit-msg.sh").exists()
    assert (hooks_dir / "mergify-hooks" / "prepare-commit-msg.sh").exists()
    assert (hooks_dir / "mergify-hooks" / "post-commit.sh").exists()


def test_setup_command_check_flag(
    git_repo_with_hooks: pathlib.Path,
) -> None:
    """Test that 'stack setup --check' shows status (alias behavior)."""
    import os

    from mergify_cli.cli import cli

    os.chdir(git_repo_with_hooks)

    runner = CliRunner()
    result = runner.invoke(cli, ["stack", "setup", "--check"])

    assert result.exit_code == 0
    assert "Git Hooks Status:" in result.output
    assert "commit-msg:" in result.output


def test_setup_command_without_flags(
    tmp_path: pathlib.Path,
) -> None:
    """Test that 'stack setup' installs hooks (backwards compatibility)."""
    import os

    from mergify_cli.cli import cli

    # Create a git repo without hooks
    subprocess.run(
        ["git", "init", "--initial-branch=main"],
        check=True,
        cwd=tmp_path,
    )
    subprocess.run(
        ["git", "config", "user.email", "test@example.com"],
        check=True,
        cwd=tmp_path,
    )
    subprocess.run(
        ["git", "config", "user.name", "Test User"],
        check=True,
        cwd=tmp_path,
    )

    os.chdir(tmp_path)

    runner = CliRunner()
    result = runner.invoke(cli, ["stack", "setup"])

    assert result.exit_code == 0

    # Verify hooks were installed
    hooks_dir = tmp_path / ".git" / "hooks"
    assert (hooks_dir / "commit-msg").exists()
    assert (hooks_dir / "prepare-commit-msg").exists()
    assert (hooks_dir / "post-commit").exists()
    assert (hooks_dir / "mergify-hooks" / "post-commit.sh").exists()


def test_post_commit_adds_missing_change_id(
    git_repo_with_hooks: pathlib.Path,
) -> None:
    """Test that the post-commit hook adds a Change-Id when commit-msg is bypassed.

    When --no-verify is used, the commit-msg hook doesn't run, but the
    post-commit hook still fires and should add the missing Change-Id.
    """
    (git_repo_with_hooks / "file.txt").write_text("content")
    subprocess.run(["git", "add", "file.txt"], check=True, cwd=git_repo_with_hooks)
    subprocess.run(
        ["git", "commit", "--no-verify", "-m", "Commit bypassing hooks"],
        check=True,
        cwd=git_repo_with_hooks,
    )

    message = get_commit_message(git_repo_with_hooks)
    change_id = get_change_id(message)

    assert change_id is not None, (
        f"Expected post-commit hook to add Change-Id:\n{message}"
    )
    assert re.match(r"^I[0-9a-f]{40}$", change_id)


def test_reset_and_recreate_preserves_change_id(
    git_repo_with_hooks: pathlib.Path,
) -> None:
    """Test that resetting to main and recreating commits preserves Change-Ids.

    This is the core Claude Code pattern: Claude resets the branch to main
    and recreates the same stack from scratch. The commit-msg hook should
    find the previous Change-Id in the branch reflog and reuse it.
    """
    # Create an initial commit on main so we can reset to it later
    (git_repo_with_hooks / "base.txt").write_text("base")
    subprocess.run(["git", "add", "base.txt"], check=True, cwd=git_repo_with_hooks)
    subprocess.run(
        ["git", "commit", "-m", "initial base"],
        check=True,
        cwd=git_repo_with_hooks,
    )

    # Create a stack commit on a branch
    subprocess.run(
        ["git", "checkout", "-b", "feat/test-stack"],
        check=True,
        cwd=git_repo_with_hooks,
    )
    (git_repo_with_hooks / "file1.txt").write_text("content1")
    subprocess.run(["git", "add", "file1.txt"], check=True, cwd=git_repo_with_hooks)
    subprocess.run(
        ["git", "commit", "-m", "feat: add feature X"],
        check=True,
        cwd=git_repo_with_hooks,
    )

    original_change_id = get_change_id(get_commit_message(git_repo_with_hooks))
    assert original_change_id is not None

    # Reset to main (simulating Claude's reset-and-recreate pattern)
    subprocess.run(
        ["git", "reset", "--hard", "main"],
        check=True,
        cwd=git_repo_with_hooks,
    )

    # Recreate the same commit with the same subject line
    (git_repo_with_hooks / "file1.txt").write_text("content1-v2")
    subprocess.run(["git", "add", "file1.txt"], check=True, cwd=git_repo_with_hooks)
    subprocess.run(
        ["git", "commit", "-m", "feat: add feature X"],
        check=True,
        cwd=git_repo_with_hooks,
    )

    recreated_change_id = get_change_id(get_commit_message(git_repo_with_hooks))
    assert recreated_change_id is not None, "Recreated commit should have a Change-Id"
    assert recreated_change_id == original_change_id, (
        f"Change-Id should be preserved when recreating a commit with the same subject.\n"
        f"Original: {original_change_id}\n"
        f"Recreated: {recreated_change_id}"
    )


def test_duplicate_subject_gets_unique_change_ids(
    git_repo_with_hooks: pathlib.Path,
) -> None:
    """Test that two commits with the same subject get different Change-Ids.

    The reflog search must NOT reuse a Change-Id from a commit that is still
    in the current branch. Otherwise two commits in the same stack would share
    a Change-Id, breaking PR tracking.
    """
    (git_repo_with_hooks / "file1.txt").write_text("content1")
    subprocess.run(["git", "add", "file1.txt"], check=True, cwd=git_repo_with_hooks)
    subprocess.run(
        ["git", "commit", "-m", "fix: typo"],
        check=True,
        cwd=git_repo_with_hooks,
    )

    first_change_id = get_change_id(get_commit_message(git_repo_with_hooks))
    assert first_change_id is not None

    # Create a second commit with the exact same subject
    (git_repo_with_hooks / "file2.txt").write_text("content2")
    subprocess.run(["git", "add", "file2.txt"], check=True, cwd=git_repo_with_hooks)
    subprocess.run(
        ["git", "commit", "-m", "fix: typo"],
        check=True,
        cwd=git_repo_with_hooks,
    )

    second_change_id = get_change_id(get_commit_message(git_repo_with_hooks))
    assert second_change_id is not None
    assert second_change_id != first_change_id, (
        f"Two commits with the same subject must get different Change-Ids.\n"
        f"First:  {first_change_id}\n"
        f"Second: {second_change_id}"
    )


async def test_setup_configures_notes_display_ref(
    git_repo_with_hooks: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """`stack setup` writes notes.displayRef = refs/notes/mergify once."""
    monkeypatch.chdir(git_repo_with_hooks)
    await setup.stack_setup()

    value = subprocess.check_output(
        ["git", "config", "--local", "--get", "notes.displayRef"],
        text=True,
        cwd=git_repo_with_hooks,
    ).strip()
    assert value == "refs/notes/mergify/*"


async def test_setup_notes_display_ref_idempotent(
    git_repo_with_hooks: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Running setup twice does not error and keeps the value."""
    monkeypatch.chdir(git_repo_with_hooks)
    await setup.stack_setup()
    await setup.stack_setup()

    value = subprocess.check_output(
        ["git", "config", "--local", "--get", "notes.displayRef"],
        text=True,
        cwd=git_repo_with_hooks,
    ).strip()
    assert value == "refs/notes/mergify/*"
