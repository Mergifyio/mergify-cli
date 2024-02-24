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

import json
import pathlib
import typing
from unittest import mock

import pytest
import respx

import mergify_cli
from mergify_cli.tests import utils as test_utils


@pytest.fixture(autouse=True)
def change_working_directory(
    monkeypatch: pytest.MonkeyPatch, tmp_path: pathlib.Path
) -> None:
    # Change working directory to avoid doing git commands in the current
    # repository
    monkeypatch.chdir(tmp_path)


@pytest.fixture
def git_mock(
    tmp_path: pathlib.Path,
) -> typing.Generator[test_utils.GitMock, None, None]:
    git_mock_object = test_utils.GitMock()
    # Top level directory is a temporary path
    git_mock_object.mock("rev-parse --show-toplevel", str(tmp_path))
    # Name of the current branch
    git_mock_object.mock("rev-parse --abbrev-ref HEAD", "current-branch")
    # URL of the GitHub repository
    git_mock_object.mock(
        "config --get remote.origin.url", "https://github.com/user/repo"
    )
    # Mock pull and push commands
    git_mock_object.mock("pull --rebase origin main", "")
    git_mock_object.mock("push -f origin current-branch:/current-branch/aio", "")

    with mock.patch("mergify_cli.git", git_mock_object):
        yield git_mock_object


def test_cli_help(capsys: pytest.CaptureFixture[str]) -> None:
    with pytest.raises(SystemExit, match="0"):
        mergify_cli.parse_args(["--help"])

    stdout = capsys.readouterr().out
    assert "usage: " in stdout
    assert "positional arguments:" in stdout
    assert "options:" in stdout


@pytest.mark.parametrize(
    "valid_branch_name",
    (
        ("my-branch"),
        ("prefix/my-branch"),
        ("my-branch/I29617d37762fd69809c255d7e7073cb11f8fbf50"),
    ),
)
def test_check_local_branch_valid(valid_branch_name: str) -> None:
    # Should not raise an error
    mergify_cli.check_local_branch(
        branch_name=valid_branch_name, branch_prefix="prefix"
    )


def test_check_local_branch_invalid() -> None:
    with pytest.raises(
        mergify_cli.LocalBranchInvalid,
        match="Local branch is a branch generated by Mergify CLI",
    ):
        mergify_cli.check_local_branch(
            branch_name="prefix/my-branch/I29617d37762fd69809c255d7e7073cb11f8fbf50",
            branch_prefix="prefix",
        )


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_stack_create(
    git_mock: test_utils.GitMock, respx_mock: respx.MockRouter
) -> None:
    # Mock 2 commits on branch `current-branch`
    git_mock.commit(
        test_utils.Commit(
            sha="commit1_sha",
            title="Title commit 1",
            message="Message commit 1",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf50",
        )
    )
    git_mock.commit(
        test_utils.Commit(
            sha="commit2_sha",
            title="Title commit 2",
            message="Message commit 2",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf51",
        )
    )

    # Mock HTTP calls
    respx_mock.get("/repos/user/repo/git/matching-refs/heads//current-branch/").respond(
        200, json=[]
    )
    respx_mock.post("/repos/user/repo/git/refs").respond(200, json={})
    post_pull1_mock = respx_mock.post(
        "/repos/user/repo/pulls", json__title="Title commit 1"
    ).respond(
        200,
        json={
            "html_url": "https://github.com/repo/user/pull/1",
            "number": "1",
            "title": "Title commit 1",
            "head": {"sha": "commit1_sha"},
            "state": "open",
            "draft": False,
            "node_id": "",
        },
    )
    post_pull2_mock = respx_mock.post(
        "/repos/user/repo/pulls", json__title="Title commit 2"
    ).respond(
        200,
        json={
            "html_url": "https://github.com/repo/user/pull/2",
            "number": "2",
            "title": "Title commit 2",
            "head": {"sha": "commit2_sha"},
            "state": "open",
            "draft": False,
            "node_id": "",
        },
    )
    respx_mock.get("/repos/user/repo/issues/1/comments").respond(200, json=[])
    post_comment1_mock = respx_mock.post("/repos/user/repo/issues/1/comments").respond(
        200
    )
    respx_mock.get("/repos/user/repo/issues/2/comments").respond(200, json=[])
    post_comment2_mock = respx_mock.post("/repos/user/repo/issues/2/comments").respond(
        200
    )

    await mergify_cli.stack(
        token="",
        next_only=False,
        branch_prefix="",
        dry_run=False,
        trunk=("origin", "main"),
    )

    # First pull request is created
    assert len(post_pull1_mock.calls) == 1
    assert json.loads(post_pull1_mock.calls.last.request.content) == {
        "head": "/current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf50",
        "base": "main",
        "title": "Title commit 1",
        "body": "Message commit 1",
        "draft": False,
    }

    # Second pull request is created
    assert len(post_pull2_mock.calls) == 1
    assert json.loads(post_pull2_mock.calls.last.request.content) == {
        "head": "/current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf51",
        "base": "/current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf50",
        "title": "Title commit 2",
        "body": "Message commit 2\n\nDepends-On: #1",
        "draft": False,
    }

    # First stack comment is created
    assert len(post_comment1_mock.calls) == 1
    expected_body = """This pull request is part of a stack:
1. Title commit 1 ([#1](https://github.com/repo/user/pull/1)) 👈
1. Title commit 2 ([#2](https://github.com/repo/user/pull/2))
"""
    assert json.loads(post_comment1_mock.calls.last.request.content) == {
        "body": expected_body
    }

    # Second stack comment is created
    assert len(post_comment2_mock.calls) == 1
    expected_body = """This pull request is part of a stack:
1. Title commit 1 ([#1](https://github.com/repo/user/pull/1))
1. Title commit 2 ([#2](https://github.com/repo/user/pull/2)) 👈
"""
    assert json.loads(post_comment2_mock.calls.last.request.content) == {
        "body": expected_body
    }


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_stack_update(
    git_mock: test_utils.GitMock, respx_mock: respx.MockRouter
) -> None:
    # Mock 1 commits on branch `current-branch`
    git_mock.commit(
        test_utils.Commit(
            sha="commit_sha",
            title="Title",
            message="Message",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf50",
        )
    )

    # Mock HTTP calls: the stack already exists but it's out of date, it should
    # be updated
    respx_mock.get("/repos/user/repo/git/matching-refs/heads//current-branch/").respond(
        200,
        json=[
            {
                "ref": "refs/heads//current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf50"
            }
        ],
    )
    respx_mock.get(
        "/repos/user/repo/pulls?head=user:/current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf50&state=open"
    ).respond(
        200,
        json=[
            {
                "html_url": "",
                "number": "123",
                "title": "Title",
                "head": {"sha": "previous_commit_sha"},
                "state": "open",
                "draft": False,
                "node_id": "",
            }
        ],
    )
    respx_mock.patch(
        "/repos/user/repo/git/refs/heads//current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf50"
    ).respond(200, json={})
    patch_pull_mock = respx_mock.patch("/repos/user/repo/pulls/123").respond(
        200, json={}
    )
    respx_mock.get("/repos/user/repo/issues/123/comments").respond(
        200,
        json=[
            {
                "body": "This pull request is part of a stack:\n...",
                "url": "https://api.github.com/repos/user/repo/issues/comments/456",
            }
        ],
    )
    respx_mock.patch("/repos/user/repo/issues/comments/456").respond(200)

    await mergify_cli.stack(
        token="",
        next_only=False,
        branch_prefix="",
        dry_run=False,
        trunk=("origin", "main"),
    )

    # The pull request is updated
    assert len(patch_pull_mock.calls) == 1
    assert json.loads(patch_pull_mock.calls.last.request.content) == {
        "head": "/current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf50",
        "base": "main",
        "title": "Title",
        "body": "Message",
    }


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_stack_on_destination_branch_raises_an_error(
    git_mock: test_utils.GitMock,
) -> None:
    git_mock.mock("rev-parse --abbrev-ref HEAD", "main")

    with pytest.raises(SystemExit, match="1"):
        await mergify_cli.stack(
            token="",
            next_only=False,
            branch_prefix="",
            dry_run=False,
            trunk=("origin", "main"),
        )


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_stack_without_common_commit_raises_an_error(
    git_mock: test_utils.GitMock,
) -> None:
    git_mock.mock("merge-base --fork-point origin/main", "")

    with pytest.raises(SystemExit, match="1"):
        await mergify_cli.stack(
            token="",
            next_only=False,
            branch_prefix="",
            dry_run=False,
            trunk=("origin", "main"),
        )
