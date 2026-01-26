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

import json
from typing import TYPE_CHECKING

import pytest

from mergify_cli.stack import list as stack_list_mod
from mergify_cli.tests import utils as test_utils


if TYPE_CHECKING:
    import respx


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_stack_list_with_prs(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Test listing a stack with commits that have associated PRs."""
    # Add required git config mock
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")

    # Mock 2 commits on branch `current-branch`
    git_mock.commit(
        test_utils.Commit(
            sha="commit1_sha",
            title="Add user authentication",
            message="Message commit 1",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf50",
        ),
    )
    git_mock.commit(
        test_utils.Commit(
            sha="commit2_sha",
            title="Implement login form",
            message="Message commit 2",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf51",
        ),
    )

    # Mock HTTP calls
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
            "title": "Add user authentication",
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
            "title": "Implement login form",
            "head": {
                "sha": "commit2_sha",
                "ref": "current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf51",
            },
            "state": "open",
            "merged_at": None,
            "draft": True,
            "node_id": "",
        },
    )

    await stack_list_mod.stack_list(
        github_server="https://api.github.com/",
        token="",
        trunk=("origin", "main"),
    )

    captured = capsys.readouterr()
    assert "current-branch" in captured.out
    assert "origin/main" in captured.out
    assert "#123" in captured.out
    assert "Add user authentication" in captured.out
    assert "#124" in captured.out
    assert "Implement login form" in captured.out


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_stack_list_no_prs(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Test listing a stack with commits that have no PRs yet."""
    # Add required git config mock
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")

    git_mock.commit(
        test_utils.Commit(
            sha="commit1_sha",
            title="Add logout functionality",
            message="Message commit 1",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf50",
        ),
    )

    # Mock HTTP calls - no PRs found
    respx_mock.get("/user").respond(200, json={"login": "author"})
    respx_mock.get("/search/issues").respond(200, json={"items": []})

    await stack_list_mod.stack_list(
        github_server="https://api.github.com/",
        token="",
        trunk=("origin", "main"),
    )

    captured = capsys.readouterr()
    assert "current-branch" in captured.out
    assert "Add logout functionality" in captured.out
    assert "no PR" in captured.out


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_stack_list_mixed_pr_states(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Test listing a stack with mixed PR states (open, draft, merged)."""
    # Add required git config mock
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")

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
    git_mock.commit(
        test_utils.Commit(
            sha="commit3_sha",
            title="Third commit",
            message="Message 3",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf52",
        ),
    )

    respx_mock.get("/user").respond(200, json={"login": "author"})
    respx_mock.get("/search/issues").respond(
        200,
        json={
            "items": [
                {
                    "pull_request": {
                        "url": "https://api.github.com/repos/user/repo/pulls/1",
                    },
                },
                {
                    "pull_request": {
                        "url": "https://api.github.com/repos/user/repo/pulls/2",
                    },
                },
                {
                    "pull_request": {
                        "url": "https://api.github.com/repos/user/repo/pulls/3",
                    },
                },
            ],
        },
    )
    # First PR: merged
    respx_mock.get("/repos/user/repo/pulls/1").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/1",
            "number": "1",
            "title": "First commit",
            "head": {
                "sha": "commit1_sha",
                "ref": "current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf50",
            },
            "state": "closed",
            "merged_at": "2024-01-01T00:00:00Z",
            "draft": False,
            "node_id": "",
        },
    )
    # Second PR: draft
    respx_mock.get("/repos/user/repo/pulls/2").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/2",
            "number": "2",
            "title": "Second commit",
            "head": {
                "sha": "commit2_sha",
                "ref": "current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf51",
            },
            "state": "open",
            "merged_at": None,
            "draft": True,
            "node_id": "",
        },
    )
    # Third PR: open
    respx_mock.get("/repos/user/repo/pulls/3").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/3",
            "number": "3",
            "title": "Third commit",
            "head": {
                "sha": "commit3_sha",
                "ref": "current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf52",
            },
            "state": "open",
            "merged_at": None,
            "draft": False,
            "node_id": "",
        },
    )

    await stack_list_mod.stack_list(
        github_server="https://api.github.com/",
        token="",
        trunk=("origin", "main"),
    )

    captured = capsys.readouterr()
    assert "merged" in captured.out
    assert "draft" in captured.out
    assert "open" in captured.out


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_stack_list_json_output(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Test JSON output format."""
    # Add required git config mock
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")

    git_mock.commit(
        test_utils.Commit(
            sha="commit1_sha",
            title="Add feature",
            message="Message",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf50",
        ),
    )

    respx_mock.get("/user").respond(200, json={"login": "author"})
    respx_mock.get("/search/issues").respond(
        200,
        json={
            "items": [
                {
                    "pull_request": {
                        "url": "https://api.github.com/repos/user/repo/pulls/42",
                    },
                },
            ],
        },
    )
    respx_mock.get("/repos/user/repo/pulls/42").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/42",
            "number": "42",
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

    await stack_list_mod.stack_list(
        github_server="https://api.github.com/",
        token="",
        trunk=("origin", "main"),
        output_json=True,
    )

    captured = capsys.readouterr()
    output = json.loads(captured.out)

    assert output["branch"] == "current-branch"
    assert output["trunk"] == "origin/main"
    assert len(output["entries"]) == 1
    assert output["entries"][0]["commit_sha"] == "commit1_sha"
    assert output["entries"][0]["title"] == "Add feature"
    assert output["entries"][0]["status"] == "open"
    assert output["entries"][0]["pull_number"] == 42
    assert output["entries"][0]["pull_url"] == "https://github.com/user/repo/pull/42"


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_stack_list_empty_stack(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Test listing an empty stack (no commits)."""
    # Add required git config mock
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")

    # Don't add any commits - just set up the base mocks
    git_mock.mock("merge-base", "--fork-point", "origin/main", output="base_commit_sha")
    git_mock.mock(
        "log",
        "--format=%H",
        "base_commit_sha..current-branch",
        output="",
    )

    respx_mock.get("/user").respond(200, json={"login": "author"})
    respx_mock.get("/search/issues").respond(200, json={"items": []})

    await stack_list_mod.stack_list(
        github_server="https://api.github.com/",
        token="",
        trunk=("origin", "main"),
    )

    captured = capsys.readouterr()
    assert "No commits in stack" in captured.out


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_stack_list_on_trunk_branch_raises_error(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
) -> None:
    """Test that listing on trunk branch raises an error."""
    # Add required git config mock
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")

    respx_mock.get("/user").respond(200, json={"login": "author"})
    git_mock.mock("rev-parse", "--abbrev-ref", "HEAD", output="main")
    git_mock.mock(
        "remote",
        "get-url",
        "origin",
        output="https://github.com/foo/bar.git",
    )

    with pytest.raises(SystemExit, match="1"):
        await stack_list_mod.stack_list(
            github_server="https://api.github.com/",
            token="",
            trunk=("origin", "main"),
        )


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_stack_list_on_generated_branch_raises_error(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
) -> None:
    """Test that listing on a generated stack branch raises an error."""
    # Add required git config mock - use "stack/author" prefix to match generated branch pattern
    git_mock.mock(
        "config",
        "--get",
        "mergify-cli.stack-branch-prefix",
        output="stack/author",
    )

    respx_mock.get("/user").respond(200, json={"login": "author"})
    # Simulate being on a generated branch
    git_mock.mock(
        "rev-parse",
        "--abbrev-ref",
        "HEAD",
        output="stack/author/my-branch/I29617d37762fd69809c255d7e7073cb11f8fbf50",
    )

    with pytest.raises(SystemExit, match="1"):
        await stack_list_mod.stack_list(
            github_server="https://api.github.com/",
            token="",
            trunk=("origin", "main"),
        )


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_stack_list_no_fork_point_raises_error(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
) -> None:
    """Test that missing fork point raises an error."""
    # Add required git config mock
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")

    respx_mock.get("/user").respond(200, json={"login": "author"})
    git_mock.mock("merge-base", "--fork-point", "origin/main", output="")

    with pytest.raises(SystemExit, match="1"):
        await stack_list_mod.stack_list(
            github_server="https://api.github.com/",
            token="",
            trunk=("origin", "main"),
        )
