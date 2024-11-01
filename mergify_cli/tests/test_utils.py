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


import collections
from unittest import mock

import pytest

from mergify_cli import utils


@pytest.mark.usefixtures("_git_repo")
async def test_get_branch_name() -> None:
    assert await utils.git_get_branch_name() == "main"


@pytest.mark.usefixtures("_git_repo")
async def test_get_target_branch() -> None:
    assert await utils.git_get_target_branch("main") == "main"


@pytest.mark.usefixtures("_git_repo")
async def test_get_target_remote() -> None:
    assert await utils.git_get_target_remote("main") == "origin"


@pytest.mark.usefixtures("_git_repo")
async def test_get_trunk() -> None:
    assert await utils.get_trunk() == "origin/main"


@pytest.mark.parametrize(
    ("default_arg_fct", "config_get_result", "expected_default"),
    [
        (utils.get_default_keep_pr_title_body, "true", True),
        (
            lambda: utils.get_default_branch_prefix("author"),
            "dummy-prefix",
            "dummy-prefix",
        ),
    ],
)
async def test_defaults_config_args_set(
    default_arg_fct: collections.abc.Callable[
        [],
        collections.abc.Awaitable[bool | str],
    ],
    config_get_result: bytes,
    expected_default: bool,
) -> None:
    with mock.patch.object(utils, "run_command", return_value=config_get_result):
        assert (await default_arg_fct()) == expected_default
