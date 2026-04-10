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

from mergify_cli.stack import checkout as stack_checkout_mod


if TYPE_CHECKING:
    import respx

    from mergify_cli.tests import utils as test_utils


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_stack_checkout_no_prs(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
) -> None:
    """Test that checkout exits cleanly when no stacked PRs are found."""
    git_mock.mock(
        "config",
        "--get",
        "mergify-cli.stack-branch-prefix",
        output="",
    )
    respx_mock.get("/search/issues").respond(200, json={"items": []})

    with pytest.raises(SystemExit, match="0"):
        await stack_checkout_mod.stack_checkout(
            github_server="https://api.github.com/",
            token="",
            user="user",
            repo="repo",
            branch_prefix=None,
            branch="my-branch",
            author="author",
            trunk=("origin", "main"),
            dry_run=True,
        )


async def test_stack_checkout_repository_from_remote(
    git_mock: test_utils.GitMock,
) -> None:
    """Test that the CLI checkout function derives user/repo from git remote when --repository is not provided."""
    git_mock.mock(
        "config",
        "--get",
        "remote.origin.url",
        output="https://github.com/myorg/myrepo.git",
    )

    with mock.patch(
        "mergify_cli.stack.cli.stack_checkout_mod.stack_checkout",
    ) as mock_checkout:
        mock_checkout.return_value = None

        from mergify_cli.stack.cli import checkout

        # Access the original async function through the decorator chain:
        # click.pass_context -> run_with_asyncio -> async def
        assert checkout.callback is not None
        checkout_async = checkout.callback.__wrapped__.__wrapped__  # type: ignore[attr-defined]

        ctx = mock.MagicMock()
        ctx.obj = {
            "github_server": "https://api.github.com/",
            "token": "test-token",
        }

        await checkout_async(
            ctx,
            author="author",
            repository=None,
            branch="my-branch",
            branch_prefix="prefix",
            dry_run=True,
            trunk=("origin", "main"),
        )

        mock_checkout.assert_called_once_with(
            "https://api.github.com/",
            "test-token",
            user="myorg",
            repo="myrepo",
            branch_prefix="prefix",
            branch="my-branch",
            author="author",
            trunk=("origin", "main"),
            dry_run=True,
        )


async def test_stack_checkout_repository_explicit(
    git_mock: test_utils.GitMock,
) -> None:
    """Test that checkout uses the explicit --repository value when provided."""
    with mock.patch(
        "mergify_cli.stack.cli.stack_checkout_mod.stack_checkout",
    ) as mock_checkout:
        mock_checkout.return_value = None

        from mergify_cli.stack.cli import checkout

        assert checkout.callback is not None
        checkout_async = checkout.callback.__wrapped__.__wrapped__  # type: ignore[attr-defined]

        ctx = mock.MagicMock()
        ctx.obj = {
            "github_server": "https://api.github.com/",
            "token": "test-token",
        }

        await checkout_async(
            ctx,
            author="author",
            repository="explicit-owner/explicit-repo",
            branch="my-branch",
            branch_prefix="prefix",
            dry_run=True,
            trunk=("origin", "main"),
        )

        mock_checkout.assert_called_once_with(
            "https://api.github.com/",
            "test-token",
            user="explicit-owner",
            repo="explicit-repo",
            branch_prefix="prefix",
            branch="my-branch",
            author="author",
            trunk=("origin", "main"),
            dry_run=True,
        )

    # git remote URL should NOT have been queried
    assert not git_mock.has_been_called_with(
        "config",
        "--get",
        "remote.origin.url",
    )


@pytest.mark.respx(base_url="https://api.github.com/")
@pytest.mark.parametrize(
    ("branch_input", "expected_branch"),
    [
        # Full head ref with prefix and change ID suffix → stripped to plain branch
        (
            "devs/JulianMaurin/MRGFY-6797/Ibb431d523fb75f48f387a3964d2936ada933cffe",
            "MRGFY-6797",
        ),
        # Change ID suffix only → stripped
        (
            "MRGFY-6797/Ibb431d523fb75f48f387a3964d2936ada933cffe",
            "MRGFY-6797",
        ),
        # Prefix only → stripped
        (
            "devs/JulianMaurin/MRGFY-6797",
            "MRGFY-6797",
        ),
        # Already plain → unchanged
        (
            "MRGFY-6797",
            "MRGFY-6797",
        ),
    ],
    ids=[
        "full-head-ref",
        "with-suffix-no-prefix",
        "with-prefix-no-suffix",
        "plain-branch",
    ],
)
async def test_stack_checkout_branch_normalization(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
    branch_input: str,
    expected_branch: str,
) -> None:
    """Test that checkout normalizes --branch by stripping prefix and change ID suffix."""
    git_mock.mock(
        "config",
        "--get",
        "mergify-cli.stack-branch-prefix",
        output="devs/JulianMaurin",
    )

    search_mock = respx_mock.get("/search/issues").respond(
        200,
        json={"items": []},
    )

    with pytest.raises(SystemExit, match="0"):
        await stack_checkout_mod.stack_checkout(
            github_server="https://api.github.com/",
            token="",
            user="user",
            repo="repo",
            branch_prefix=None,
            branch=branch_input,
            author="author",
            trunk=("origin", "main"),
            dry_run=True,
        )

    # Verify the search query: stack_branch = prefix/normalized_branch
    expected_stack_branch = f"devs/JulianMaurin/{expected_branch}"
    assert len(search_mock.calls) == 1
    query = str(search_mock.calls[0].request.url)
    assert f"head%3A{expected_stack_branch.replace('/', '%2F')}%2F" in query
