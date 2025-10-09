import subprocess
from unittest import mock

import click
import pytest

from mergify_cli.ci.scopes import changed_files


@mock.patch("mergify_cli.ci.scopes.changed_files.subprocess.check_output")
def test_git_changed_files(mock_subprocess: mock.Mock) -> None:
    mock_subprocess.return_value = "file1.py\nfile2.js\n"

    result = changed_files.git_changed_files("main")

    mock_subprocess.assert_called_once_with(
        ["git", "diff", "--name-only", "--diff-filter=ACMRTD", "main...HEAD"],
        text=True,
        encoding="utf-8",
    )
    assert result == ["file1.py", "file2.js"]


@mock.patch("mergify_cli.ci.scopes.changed_files.subprocess.check_output")
def test_git_changed_files_empty(mock_subprocess: mock.Mock) -> None:
    mock_subprocess.return_value = ""

    result = changed_files.git_changed_files("main")

    assert result == []


@mock.patch("mergify_cli.ci.scopes.changed_files.subprocess.check_output")
def test_run_command_failure(mock_subprocess: mock.Mock) -> None:
    mock_subprocess.side_effect = subprocess.CalledProcessError(1, ["git", "diff"])

    with pytest.raises(click.ClickException, match="Command failed"):
        changed_files._run(["git", "diff"])
