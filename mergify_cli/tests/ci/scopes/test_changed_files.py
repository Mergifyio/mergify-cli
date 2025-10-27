from __future__ import annotations

from typing import TYPE_CHECKING

import pytest

from mergify_cli.ci.scopes import changed_files


if TYPE_CHECKING:
    from mergify_cli.tests import utils as test_utils


def test_git_changed_files(mock_subprocess: test_utils.SubprocessMocks) -> None:
    mock_subprocess.register(["git", "merge-base", "main", "HEAD"])
    mock_subprocess.register(["git", "rev-list", "--count", "--all"], "100")
    mock_subprocess.register(["git", "merge-base", "main", "HEAD"])
    mock_subprocess.register(
        ["git", "diff", "--name-only", "--diff-filter=ACMRTD", "main...HEAD"],
        "file1.py\nfile2.js\n",
    )

    result = changed_files.git_changed_files("main", "HEAD")

    assert result == ["file1.py", "file2.js"]


def test_git_changed_files_fetch_alot_of_history(
    mock_subprocess: test_utils.SubprocessMocks,
) -> None:
    base = "b3deb84c4befe1918995b18eb06fa05f9074636d"
    head = "9b6d25af10e6285862eb2476106f266d2aa303cf"

    mock_subprocess.register(
        ["git", "merge-base", base, head],
        "No such git object",
        1,
    )
    mock_subprocess.register(
        ["git", "fetch", "--no-tags", "--depth=100", "origin", base, head],
    )
    mock_subprocess.register(["git", "rev-list", "--count", "--all"], "100")

    # Loop until we find it
    for count in (200, 400, 800, 1600):
        mock_subprocess.register(
            ["git", "merge-base", base, head],
            "No such git object",
            1,
        )
        mock_subprocess.register(
            ["git", "fetch", f"--deepen={count}", "origin", base, head],
        )
        mock_subprocess.register(["git", "rev-list", "--count", "--all"], f"{count}")

    # We found it!
    mock_subprocess.register(["git", "merge-base", base, head])

    mock_subprocess.register(
        ["git", "diff", "--name-only", "--diff-filter=ACMRTD", f"{base}...{head}"],
        "file1.py\nfile2.js\n",
    )

    result = changed_files.git_changed_files(base, head)

    assert result == ["file1.py", "file2.js"]


def test_git_changed_files_empty(mock_subprocess: test_utils.SubprocessMocks) -> None:
    mock_subprocess.register(["git", "merge-base", "main", "HEAD"])
    mock_subprocess.register(["git", "rev-list", "--count", "--all"], "100")
    mock_subprocess.register(["git", "merge-base", "main", "HEAD"])
    mock_subprocess.register(
        ["git", "diff", "--name-only", "--diff-filter=ACMRTD", "main...HEAD"],
        "",
    )

    result = changed_files.git_changed_files("main", "HEAD")

    assert result == []


def test_run_command_failure(mock_subprocess: test_utils.SubprocessMocks) -> None:
    mock_subprocess.register(["git", "merge-base", "main", "HEAD"])
    mock_subprocess.register(["git", "rev-list", "--count", "--all"], "100")
    mock_subprocess.register(["git", "merge-base", "main", "HEAD"])
    mock_subprocess.register(
        ["git", "diff", "--name-only", "--diff-filter=ACMRTD", "main...HEAD"],
        "No such git object",
        1,
    )

    with pytest.raises(changed_files.ChangedFilesError, match="Command failed"):
        changed_files.git_changed_files("main", "HEAD")
