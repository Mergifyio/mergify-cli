from __future__ import annotations

from typing import TYPE_CHECKING

import pytest

from mergify_cli.ci.scopes import changed_files


if TYPE_CHECKING:
    from mergify_cli.tests import utils as test_utils


def test_git_changed_files(mock_subprocess: test_utils.SubprocessMocks) -> None:
    mock_subprocess.register(["git", "merge-base", "--", "main", "HEAD"])
    mock_subprocess.register(
        [
            "git",
            "diff",
            "--name-only",
            "--diff-filter=ACMRTD",
            "main...HEAD",
            "--",
        ],
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
        ["git", "merge-base", "--", base, head],
        "No such git object",
        1,
    )
    mock_subprocess.register(
        ["git", "fetch", "--no-tags", "--depth=100", "origin", "--", base, head],
    )
    mock_subprocess.register(["git", "rev-list", "--count", "--all"], "100")

    # Loop until we find it
    for count in (200, 400, 800, 1600):
        mock_subprocess.register(
            ["git", "merge-base", "--", base, head],
            "No such git object",
            1,
        )
        mock_subprocess.register(
            [
                "git",
                "fetch",
                "--no-tags",
                f"--deepen={count}",
                "origin",
                "--",
                base,
                head,
            ],
        )
        mock_subprocess.register(["git", "rev-list", "--count", "--all"], f"{count}")

    # We found it!
    mock_subprocess.register(["git", "merge-base", "--", base, head])

    mock_subprocess.register(
        [
            "git",
            "diff",
            "--name-only",
            "--diff-filter=ACMRTD",
            f"{base}...{head}",
            "--",
        ],
        "file1.py\nfile2.js\n",
    )

    result = changed_files.git_changed_files(base, head)

    assert result == ["file1.py", "file2.js"]


def test_git_changed_files_fetch_branch_name_uses_refspec(
    mock_subprocess: test_utils.SubprocessMocks,
) -> None:
    """A branch name must be fetched via refspec so it becomes a local ref.

    Without a refspec, `git fetch origin <branch>` only updates FETCH_HEAD,
    and subsequent `git merge-base <branch> ...` fails with
    "Not a valid object name".
    """
    base = "devs/sileht/test-stack/add-random-line-readme-md--861404f9"
    head = "27adb8407351454f8859bc85ac2709d13fd5e9f9"
    local_base = f"refs/mergify-cli/fetched/{base}"
    refspec = f"+{base}:{local_base}"

    mock_subprocess.register(
        ["git", "merge-base", "--", base, head],
        "Not a valid object name",
        1,
    )
    mock_subprocess.register(
        ["git", "fetch", "--no-tags", "--depth=100", "origin", "--", refspec, head],
    )
    mock_subprocess.register(["git", "rev-list", "--count", "--all"], "100")
    mock_subprocess.register(["git", "merge-base", "--", local_base, head])
    mock_subprocess.register(
        [
            "git",
            "diff",
            "--name-only",
            "--diff-filter=ACMRTD",
            f"{local_base}...{head}",
            "--",
        ],
        "file1.py\n",
    )

    result = changed_files.git_changed_files(base, head)

    assert result == ["file1.py"]


def test_git_changed_files_short_hex_branch_name_not_sha(
    mock_subprocess: test_utils.SubprocessMocks,
) -> None:
    """A branch name that happens to be valid hex but shorter than 40 chars
    must be treated as a branch, not a SHA — otherwise it would be fetched
    bare and merge-base would fail for the exact bug this module fixes."""
    base = "deadbeef"
    head = "9b6d25af10e6285862eb2476106f266d2aa303cf"
    local_base = f"refs/mergify-cli/fetched/{base}"
    refspec = f"+{base}:{local_base}"

    mock_subprocess.register(
        ["git", "merge-base", "--", base, head],
        "Not a valid object name",
        1,
    )
    mock_subprocess.register(
        ["git", "fetch", "--no-tags", "--depth=100", "origin", "--", refspec, head],
    )
    mock_subprocess.register(["git", "rev-list", "--count", "--all"], "100")
    mock_subprocess.register(["git", "merge-base", "--", local_base, head])
    mock_subprocess.register(
        [
            "git",
            "diff",
            "--name-only",
            "--diff-filter=ACMRTD",
            f"{local_base}...{head}",
            "--",
        ],
        "file.py\n",
    )

    result = changed_files.git_changed_files(base, head)

    assert result == ["file.py"]


def test_git_changed_files_shallow_local_refs_deepen(
    mock_subprocess: test_utils.SubprocessMocks,
) -> None:
    """When both refs are local (e.g. HEAD^/HEAD) and the clone is shallow,
    deepen via `git fetch --deepen` without refspecs until merge-base works.
    """
    mock_subprocess.register(
        ["git", "merge-base", "--", "HEAD^", "HEAD"],
        "fatal: bad revision 'HEAD^'",
        1,
    )
    mock_subprocess.register(
        ["git", "fetch", "--no-tags", "--depth=100", "origin"],
    )
    mock_subprocess.register(["git", "rev-list", "--count", "--all"], "50")
    mock_subprocess.register(
        ["git", "merge-base", "--", "HEAD^", "HEAD"],
        "fatal: bad revision 'HEAD^'",
        1,
    )
    mock_subprocess.register(
        ["git", "fetch", "--no-tags", "--deepen=200", "origin"],
    )
    mock_subprocess.register(["git", "rev-list", "--count", "--all"], "100")
    mock_subprocess.register(["git", "merge-base", "--", "HEAD^", "HEAD"])
    mock_subprocess.register(
        [
            "git",
            "diff",
            "--name-only",
            "--diff-filter=ACMRTD",
            "HEAD^...HEAD",
            "--",
        ],
        "file.py\n",
    )

    result = changed_files.git_changed_files("HEAD^", "HEAD")

    assert result == ["file.py"]


def test_git_changed_files_empty(mock_subprocess: test_utils.SubprocessMocks) -> None:
    mock_subprocess.register(["git", "merge-base", "--", "main", "HEAD"])
    mock_subprocess.register(
        [
            "git",
            "diff",
            "--name-only",
            "--diff-filter=ACMRTD",
            "main...HEAD",
            "--",
        ],
        "",
    )

    result = changed_files.git_changed_files("main", "HEAD")

    assert result == []


def test_run_command_failure(mock_subprocess: test_utils.SubprocessMocks) -> None:
    mock_subprocess.register(["git", "merge-base", "--", "main", "HEAD"])
    mock_subprocess.register(
        [
            "git",
            "diff",
            "--name-only",
            "--diff-filter=ACMRTD",
            "main...HEAD",
            "--",
        ],
        "No such git object",
        1,
    )

    with pytest.raises(changed_files.ChangedFilesError, match="Command failed"):
        changed_files.git_changed_files("main", "HEAD")
