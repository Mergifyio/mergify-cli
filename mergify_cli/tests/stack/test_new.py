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

from typing import TYPE_CHECKING
from unittest import mock

import pytest

from mergify_cli import utils
from mergify_cli.stack import new as stack_new_mod


if TYPE_CHECKING:
    from mergify_cli.tests import utils as test_utils


class TestStackNew:
    """Tests for the stack_new function."""

    async def test_stack_new_creates_branch(
        self,
        git_mock: test_utils.GitMock,
    ) -> None:
        """Should create a new branch with upstream tracking."""
        git_mock.mock(
            "config",
            "--get",
            "branch.current-branch.merge",
            output="refs/heads/main",
        )
        git_mock.mock(
            "config",
            "--get",
            "branch.current-branch.remote",
            output="origin",
        )
        git_mock.mock("fetch", "origin", "main", output="")
        git_mock.mock(
            "checkout",
            "--track",
            "-b",
            "my-feature",
            "origin/main",
            output="",
        )

        await stack_new_mod.stack_new(
            name="my-feature",
            base=None,
            checkout=True,
        )

        assert git_mock.has_been_called_with("fetch", "origin", "main")
        assert git_mock.has_been_called_with(
            "checkout",
            "--track",
            "-b",
            "my-feature",
            "origin/main",
        )

    async def test_stack_new_with_custom_base(
        self,
        git_mock: test_utils.GitMock,
    ) -> None:
        """Should create branch from custom base."""
        git_mock.mock("fetch", "upstream", "develop", output="")
        git_mock.mock(
            "checkout",
            "--track",
            "-b",
            "my-feature",
            "upstream/develop",
            output="",
        )

        await stack_new_mod.stack_new(
            name="my-feature",
            base=("upstream", "develop"),
            checkout=True,
        )

        assert git_mock.has_been_called_with("fetch", "upstream", "develop")
        assert git_mock.has_been_called_with(
            "checkout",
            "--track",
            "-b",
            "my-feature",
            "upstream/develop",
        )

    async def test_stack_new_no_checkout(
        self,
        git_mock: test_utils.GitMock,
    ) -> None:
        """Should create branch without checking out when checkout=False."""
        git_mock.mock(
            "config",
            "--get",
            "branch.current-branch.merge",
            output="refs/heads/main",
        )
        git_mock.mock(
            "config",
            "--get",
            "branch.current-branch.remote",
            output="origin",
        )
        git_mock.mock("fetch", "origin", "main", output="")
        git_mock.mock("branch", "--track", "my-feature", "origin/main", output="")

        await stack_new_mod.stack_new(
            name="my-feature",
            base=None,
            checkout=False,
        )

        assert git_mock.has_been_called_with("fetch", "origin", "main")
        assert git_mock.has_been_called_with(
            "branch",
            "--track",
            "my-feature",
            "origin/main",
        )
        assert not git_mock.has_been_called_with(
            "checkout",
            "--track",
            "-b",
            "my-feature",
            "origin/main",
        )

    async def test_stack_new_branch_already_exists(
        self,
        git_mock: test_utils.GitMock,
    ) -> None:
        """Should exit with code 1 when branch already exists."""
        git_mock.mock(
            "config",
            "--get",
            "branch.current-branch.merge",
            output="refs/heads/main",
        )
        git_mock.mock(
            "config",
            "--get",
            "branch.current-branch.remote",
            output="origin",
        )
        git_mock.mock("fetch", "origin", "main", output="")

        async def patched_git(*args: str) -> str:
            if args == ("checkout", "--track", "-b", "existing-branch", "origin/main"):
                raise utils.CommandError(
                    args,
                    128,
                    b"fatal: a branch named 'existing-branch' already exists",
                )
            return await git_mock(*args)

        with (
            mock.patch("mergify_cli.utils.git", patched_git),
            pytest.raises(SystemExit) as exc_info,
        ):
            await stack_new_mod.stack_new(
                name="existing-branch",
                base=None,
                checkout=True,
            )

        assert exc_info.value.code == 1
        assert git_mock.has_been_called_with("fetch", "origin", "main")

    async def test_stack_new_trunk_not_found(
        self,
    ) -> None:
        """Should exit with code 1 when trunk cannot be determined."""

        async def patched_get_trunk() -> str:
            raise utils.CommandError(("config", "--get"), 1, b"")

        with (
            mock.patch("mergify_cli.utils.get_trunk", patched_get_trunk),
            pytest.raises(SystemExit) as exc_info,
        ):
            await stack_new_mod.stack_new(
                name="my-feature",
                base=None,
                checkout=True,
            )

        assert exc_info.value.code == 1
