#
#  Copyright © 2021-2024 Mergify SAS
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
import pathlib
import subprocess
import typing
from unittest import mock

import pytest

from mergify_cli.tests import utils as test_utils


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
) -> typing.Generator[test_utils.GitMock, None, None]:
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
    # Mock pull and push commands
    git_mock_object.mock("pull", "--rebase", "origin", "main", output="")
    git_mock_object.mock(
        "push",
        "-f",
        "origin",
        "current-branch:current-branch/aio",
        output="",
    )

    with mock.patch("mergify_cli.utils.git", git_mock_object):
        yield git_mock_object
