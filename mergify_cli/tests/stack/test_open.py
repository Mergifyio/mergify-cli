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

from mergify_cli.stack import open as stack_open_mod
from mergify_cli.tests import utils as test_utils


if TYPE_CHECKING:
    import respx


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_stack_open_head(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
) -> None:
    """Test opening the PR for HEAD commit."""
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")
    git_mock.mock(
        "config",
        "--get",
        "branch.current-branch.merge",
        output="refs/heads/main",
    )
    git_mock.mock("config", "--get", "branch.current-branch.remote", output="origin")
    git_mock.mock("rev-parse", "HEAD", output="commit1_sha")

    git_mock.commit(
        test_utils.Commit(
            sha="commit1_sha",
            title="Add feature",
            message="Message",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf50",
        ),
    )
    git_mock.finalize()

    respx_mock.get("/user").respond(200, json={"login": "author"})
    respx_mock.get("/search/issues").respond(
        200,
        json={
            "items": [
                {
                    "pull_request": {
                        "url": "https://api.github.com/repos/user/repo/pulls/123",
                    },
                },
            ],
        },
    )
    respx_mock.get("/repos/user/repo/pulls/123").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/123",
            "number": "123",
            "title": "Add feature",
            "head": {
                "sha": "commit1_sha",
                "ref": "current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf50",
            },
            "state": "open",
            "merged_at": None,
            "draft": False,
            "node_id": "",
        },
    )

    with mock.patch("webbrowser.open") as mock_open:
        await stack_open_mod.stack_open(
            github_server="https://api.github.com/",
            token="",
            commit="HEAD",
        )
        mock_open.assert_called_once_with("https://github.com/user/repo/pull/123")


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_stack_open_specific_commit(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
) -> None:
    """Test opening the PR for a specific commit SHA."""
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")
    git_mock.mock(
        "config",
        "--get",
        "branch.current-branch.merge",
        output="refs/heads/main",
    )
    git_mock.mock("config", "--get", "branch.current-branch.remote", output="origin")
    git_mock.mock("rev-parse", "abc1234", output="commit1_sha")

    git_mock.commit(
        test_utils.Commit(
            sha="commit1_sha",
            title="First commit",
            message="Message 1",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf50",
        ),
    )
    git_mock.commit(
        test_utils.Commit(
            sha="commit2_sha",
            title="Second commit",
            message="Message 2",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf51",
        ),
    )
    git_mock.finalize()

    respx_mock.get("/user").respond(200, json={"login": "author"})
    respx_mock.get("/search/issues").respond(
        200,
        json={
            "items": [
                {
                    "pull_request": {
                        "url": "https://api.github.com/repos/user/repo/pulls/123",
                    },
                },
                {
                    "pull_request": {
                        "url": "https://api.github.com/repos/user/repo/pulls/124",
                    },
                },
            ],
        },
    )
    respx_mock.get("/repos/user/repo/pulls/123").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/123",
            "number": "123",
            "title": "First commit",
            "head": {
                "sha": "commit1_sha",
                "ref": "current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf50",
            },
            "state": "open",
            "merged_at": None,
            "draft": False,
            "node_id": "",
        },
    )
    respx_mock.get("/repos/user/repo/pulls/124").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/124",
            "number": "124",
            "title": "Second commit",
            "head": {
                "sha": "commit2_sha",
                "ref": "current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf51",
            },
            "state": "open",
            "merged_at": None,
            "draft": False,
            "node_id": "",
        },
    )

    with mock.patch("webbrowser.open") as mock_open:
        await stack_open_mod.stack_open(
            github_server="https://api.github.com/",
            token="",
            commit="abc1234",
        )
        mock_open.assert_called_once_with("https://github.com/user/repo/pull/123")


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_stack_open_no_pr(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Test error when commit has no associated PR."""
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")
    git_mock.mock(
        "config",
        "--get",
        "branch.current-branch.merge",
        output="refs/heads/main",
    )
    git_mock.mock("config", "--get", "branch.current-branch.remote", output="origin")
    git_mock.mock("rev-parse", "HEAD", output="commit1_sha")

    git_mock.commit(
        test_utils.Commit(
            sha="commit1_sha",
            title="Unpushed commit",
            message="Message",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf50",
        ),
    )
    git_mock.finalize()

    respx_mock.get("/user").respond(200, json={"login": "author"})
    respx_mock.get("/search/issues").respond(200, json={"items": []})

    with (
        mock.patch("webbrowser.open") as mock_open,
        pytest.raises(SystemExit, match="1"),
    ):
        await stack_open_mod.stack_open(
            github_server="https://api.github.com/",
            token="",
            commit="HEAD",
        )

    mock_open.assert_not_called()
    captured = capsys.readouterr()
    assert "No PR for" in captured.out
    assert "mergify stack push" in captured.out


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_stack_open_commit_not_in_stack(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Test error when commit exists but is not in the stack."""
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")
    git_mock.mock(
        "config",
        "--get",
        "branch.current-branch.merge",
        output="refs/heads/main",
    )
    git_mock.mock("config", "--get", "branch.current-branch.remote", output="origin")
    git_mock.mock("rev-parse", "other_commit", output="other_sha")

    git_mock.commit(
        test_utils.Commit(
            sha="commit1_sha",
            title="Stack commit",
            message="Message",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf50",
        ),
    )
    git_mock.finalize()

    respx_mock.get("/user").respond(200, json={"login": "author"})
    respx_mock.get("/search/issues").respond(200, json={"items": []})

    with pytest.raises(SystemExit, match="1"):
        await stack_open_mod.stack_open(
            github_server="https://api.github.com/",
            token="",
            commit="other_commit",
        )

    captured = capsys.readouterr()
    assert "not found in stack" in captured.out


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_stack_open_invalid_commit(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Test error when commit ref doesn't exist."""
    from mergify_cli import utils

    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")
    git_mock.mock(
        "config",
        "--get",
        "branch.current-branch.merge",
        output="refs/heads/main",
    )
    git_mock.mock("config", "--get", "branch.current-branch.remote", output="origin")

    git_mock.commit(
        test_utils.Commit(
            sha="commit1_sha",
            title="Stack commit",
            message="Message",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf50",
        ),
    )
    git_mock.finalize()

    respx_mock.get("/user").respond(200, json={"login": "author"})
    respx_mock.get("/search/issues").respond(200, json={"items": []})

    # Create a wrapper that raises CommandError for the specific invalid ref
    async def git_with_invalid_ref(*args: str) -> str:
        if args == ("rev-parse", "invalid_ref"):
            raise utils.CommandError(
                command_args=args,
                returncode=128,
                stdout=b"",
            )
        return await git_mock(*args)

    with (
        mock.patch("mergify_cli.utils.git", side_effect=git_with_invalid_ref),
        pytest.raises(SystemExit, match="1"),
    ):
        await stack_open_mod.stack_open(
            github_server="https://api.github.com/",
            token="",
            commit="invalid_ref",
        )

    captured = capsys.readouterr()
    assert "not found" in captured.out


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_stack_open_interactive_selection(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
) -> None:
    """Test interactive selection when no commit is specified."""
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")
    git_mock.mock(
        "config",
        "--get",
        "branch.current-branch.merge",
        output="refs/heads/main",
    )
    git_mock.mock("config", "--get", "branch.current-branch.remote", output="origin")

    git_mock.commit(
        test_utils.Commit(
            sha="commit1_sha",
            title="First commit",
            message="Message 1",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf50",
        ),
    )
    git_mock.commit(
        test_utils.Commit(
            sha="commit2_sha",
            title="Second commit",
            message="Message 2",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf51",
        ),
    )
    git_mock.finalize()

    respx_mock.get("/user").respond(200, json={"login": "author"})
    respx_mock.get("/search/issues").respond(
        200,
        json={
            "items": [
                {
                    "pull_request": {
                        "url": "https://api.github.com/repos/user/repo/pulls/123",
                    },
                },
                {
                    "pull_request": {
                        "url": "https://api.github.com/repos/user/repo/pulls/124",
                    },
                },
            ],
        },
    )
    respx_mock.get("/repos/user/repo/pulls/123").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/123",
            "number": "123",
            "title": "First commit",
            "head": {
                "sha": "commit1_sha",
                "ref": "current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf50",
            },
            "state": "open",
            "merged_at": None,
            "draft": False,
            "node_id": "",
        },
    )
    respx_mock.get("/repos/user/repo/pulls/124").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/124",
            "number": "124",
            "title": "Second commit",
            "head": {
                "sha": "commit2_sha",
                "ref": "current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf51",
            },
            "state": "open",
            "merged_at": None,
            "draft": False,
            "node_id": "",
        },
    )

    # Mock questionary.select to return the first entry
    mock_select = mock.MagicMock()
    mock_select.ask_async = mock.AsyncMock(
        return_value=mock.MagicMock(
            pull_url="https://github.com/user/repo/pull/123",
            pull_number=123,
            title="First commit",
            commit_sha="commit1_sha",
        ),
    )

    with (
        mock.patch("webbrowser.open") as mock_browser,
        mock.patch("questionary.select", return_value=mock_select),
    ):
        await stack_open_mod.stack_open(
            github_server="https://api.github.com/",
            token="",
            commit=None,
        )
        mock_browser.assert_called_once_with("https://github.com/user/repo/pull/123")
