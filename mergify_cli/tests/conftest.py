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

import shutil
import subprocess
from typing import TYPE_CHECKING
from unittest import mock

import pytest

from mergify_cli.tests import utils as test_utils


if TYPE_CHECKING:
    from collections import abc
    from collections.abc import Generator
    import pathlib


@pytest.fixture(autouse=True)
def _unset_ci(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("CI", raising=False)


@pytest.fixture(autouse=True)
def _unset_github_token(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("GITHUB_TOKEN", "whatever")


@pytest.fixture(autouse=True)
def _change_working_directory(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: pathlib.Path,
) -> None:
    # Change working directory to avoid doing git commands in the current
    # repository
    monkeypatch.chdir(tmp_path)


@pytest.fixture
def _git_repo() -> None:
    subprocess.call(["git", "init", "--initial-branch=main"])
    subprocess.call(["git", "config", "user.email", "test@example.com"])
    subprocess.call(["git", "config", "user.name", "Test User"])
    subprocess.call(["git", "commit", "--allow-empty", "-m", "Initial commit"])
    subprocess.call(["git", "config", "--add", "branch.main.merge", "refs/heads/main"])
    subprocess.call(["git", "config", "--add", "branch.main.remote", "origin"])


@pytest.fixture
def git_mock(
    tmp_path: pathlib.Path,
) -> Generator[test_utils.GitMock]:
    git_mock_object = test_utils.GitMock()
    # Top level directory is a temporary path
    git_mock_object.mock("rev-parse", "--show-toplevel", output=str(tmp_path))
    # Name of the current branch
    git_mock_object.mock("rev-parse", "--abbrev-ref", "HEAD", output="current-branch")
    # URL of the GitHub repository
    git_mock_object.mock(
        "config",
        "--get",
        "remote.origin.url",
        output="https://github.com/user/repo",
    )
    # Mock pull command
    git_mock_object.mock("pull", "--rebase", "origin", "main", output="")

    with mock.patch("mergify_cli.utils.git", git_mock_object):
        yield git_mock_object


@pytest.fixture
def mock_subprocess() -> abc.Generator[test_utils.SubprocessMocks]:
    yield from test_utils.subprocess_mocked()


@pytest.fixture
def git_repo_with_hooks(tmp_path: pathlib.Path) -> pathlib.Path:
    """Create a real git repo with the stack hooks installed (new sourcing architecture)."""
    import importlib.resources

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

    # Install hooks with new sourcing architecture
    hooks_dir = tmp_path / ".git" / "hooks"
    managed_dir = hooks_dir / "mergify-hooks"
    managed_dir.mkdir(parents=True, exist_ok=True)

    for hook_name in ("commit-msg", "prepare-commit-msg"):
        # Install wrapper
        wrapper_source = str(
            importlib.resources.files("mergify_cli.stack").joinpath(
                f"hooks/wrappers/{hook_name}",
            ),
        )
        wrapper_dest = hooks_dir / hook_name
        shutil.copy(wrapper_source, wrapper_dest)
        wrapper_dest.chmod(0o755)

        # Install managed script
        script_source = str(
            importlib.resources.files("mergify_cli.stack").joinpath(
                f"hooks/scripts/{hook_name}.sh",
            ),
        )
        script_dest = managed_dir / f"{hook_name}.sh"
        shutil.copy(script_source, script_dest)
        script_dest.chmod(0o755)

    return tmp_path
