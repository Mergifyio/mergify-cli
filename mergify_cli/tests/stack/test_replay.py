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
import typing

import httpx
import respx

from mergify_cli.stack import replay


if typing.TYPE_CHECKING:
    from mergify_cli.tests import utils as test_utils


async def test_compute_merged_tree_clean(git_mock: test_utils.GitMock) -> None:
    """Clean merge returns the merged tree SHA."""
    git_mock.mock("rev-parse", "old_sha^", output="parent_old_sha")
    git_mock.mock("rev-parse", "new_sha^", output="parent_new_sha")
    git_mock.mock(
        "merge-tree",
        "--write-tree",
        "--merge-base=parent_old_sha",
        "parent_new_sha",
        "old_sha",
        output="merged_tree_sha",
    )

    result = await replay.compute_merged_tree(old_sha="old_sha", new_sha="new_sha")

    assert result == replay.MergedTree(
        tree_sha="merged_tree_sha",
        parent_new_sha="parent_new_sha",
    )


async def test_compute_merged_tree_conflict_returns_none(
    git_mock: test_utils.GitMock,
) -> None:
    """Conflicting merge returns None."""
    git_mock.mock("rev-parse", "old_sha^", output="parent_old_sha")
    git_mock.mock("rev-parse", "new_sha^", output="parent_new_sha")
    git_mock.mock_error(
        "merge-tree",
        "--write-tree",
        "--merge-base=parent_old_sha",
        "parent_new_sha",
        "old_sha",
    )

    result = await replay.compute_merged_tree(old_sha="old_sha", new_sha="new_sha")

    assert result is None


async def test_compute_merged_tree_rev_parse_error_returns_none(
    git_mock: test_utils.GitMock,
) -> None:
    """If `git rev-parse` fails (e.g., parent not fetched), return None."""
    git_mock.mock_error("rev-parse", "old_sha^")
    # parent_new_sha rev-parse may also be attempted (concurrent with old_sha^);
    # register it so the gather doesn't hit "not mocked".
    git_mock.mock_error("rev-parse", "new_sha^")

    result = await replay.compute_merged_tree(old_sha="old_sha", new_sha="new_sha")

    assert result is None


async def test_compute_tree_delta_parses_modifications_and_deletions(
    git_mock: test_utils.GitMock,
) -> None:
    """diff-tree --raw output is converted into Git Data API tree entries."""
    raw_output = (
        ":100644 100644 aaa1111 bbb2222 M\tsrc/a.py\n"
        ":100755 000000 ccc3333 0000000 D\tscripts/exec.sh\n"
        ":000000 100755 0000000 ddd4444 A\tscripts/run.sh\n"
        ":100644 100755 eee5555 fff6666 T\tsrc/c.py\n"
    )
    git_mock.mock(
        "diff-tree",
        "-r",
        "--raw",
        "--no-renames",
        "base_tree_sha",
        "merged_tree_sha",
        output=raw_output,
    )

    entries = await replay.compute_tree_delta(
        base_tree_sha="base_tree_sha",
        merged_tree_sha="merged_tree_sha",
    )

    assert entries == [
        {"path": "src/a.py", "mode": "100644", "type": "blob", "sha": "bbb2222"},
        {"path": "scripts/exec.sh", "mode": "100755", "type": "blob", "sha": None},
        {
            "path": "scripts/run.sh",
            "mode": "100755",
            "type": "blob",
            "sha": "ddd4444",
        },
        {"path": "src/c.py", "mode": "100755", "type": "blob", "sha": "fff6666"},
    ]


async def test_compute_tree_delta_empty_when_no_diff(
    git_mock: test_utils.GitMock,
) -> None:
    """Empty diff-tree output produces an empty entry list."""
    git_mock.mock("diff-tree", "-r", "--raw", "--no-renames", "x", "y", output="")

    entries = await replay.compute_tree_delta(base_tree_sha="x", merged_tree_sha="y")

    assert entries == []


@respx.mock
async def test_upload_replay_commit_posts_tree_then_commit() -> None:
    """upload_replay_commit chains tree+commit POSTs and returns the commit SHA."""
    base_url = "https://api.github.com"
    tree_route = respx.post(f"{base_url}/repos/owner/repo/git/trees").mock(
        return_value=httpx.Response(201, json={"sha": "new_tree_server_sha"}),
    )
    commit_route = respx.post(f"{base_url}/repos/owner/repo/git/commits").mock(
        return_value=httpx.Response(201, json={"sha": "new_commit_server_sha"}),
    )
    entries: list[dict[str, str | None]] = [
        {"path": "src/a.py", "mode": "100644", "type": "blob", "sha": "bbb2222"},
    ]

    async with httpx.AsyncClient(base_url=base_url) as client:
        sha = await replay.upload_replay_commit(
            client=client,
            user="owner",
            repo="repo",
            base_tree_sha="parent_new_tree_sha",
            parent_new_sha="parent_new_sha",
            old_sha="abc1234",
            entries=entries,
        )

    assert sha == "new_commit_server_sha"
    assert tree_route.called
    tree_body = json.loads(tree_route.calls.last.request.read())
    assert tree_body["base_tree"] == "parent_new_tree_sha"
    assert tree_body["tree"] == [
        {"path": "src/a.py", "mode": "100644", "type": "blob", "sha": "bbb2222"},
    ]
    assert commit_route.called
    commit_body = json.loads(commit_route.calls.last.request.read())
    assert commit_body["tree"] == "new_tree_server_sha"
    assert commit_body["parents"] == ["parent_new_sha"]


@respx.mock
async def test_upload_replay_commit_returns_none_on_api_error() -> None:
    """API error during tree POST returns None, no commit POST attempted."""
    base_url = "https://api.github.com"
    respx.post(f"{base_url}/repos/owner/repo/git/trees").mock(
        return_value=httpx.Response(422, json={"message": "tree invalid"}),
    )

    async with httpx.AsyncClient(base_url=base_url) as client:
        sha = await replay.upload_replay_commit(
            client=client,
            user="owner",
            repo="repo",
            base_tree_sha="parent_new_tree_sha",
            parent_new_sha="parent_new_sha",
            old_sha="abc1234",
            entries=[],
        )

    assert sha is None


@respx.mock
async def test_upload_replay_commit_returns_none_on_commit_post_failure() -> None:
    """API error during commit POST returns None (after tree POST succeeded)."""
    base_url = "https://api.github.com"
    respx.post(f"{base_url}/repos/owner/repo/git/trees").mock(
        return_value=httpx.Response(201, json={"sha": "new_tree_server_sha"}),
    )
    respx.post(f"{base_url}/repos/owner/repo/git/commits").mock(
        return_value=httpx.Response(422, json={"message": "commit invalid"}),
    )

    async with httpx.AsyncClient(base_url=base_url) as client:
        sha = await replay.upload_replay_commit(
            client=client,
            user="owner",
            repo="repo",
            base_tree_sha="parent_new_tree_sha",
            parent_new_sha="parent_new_sha",
            old_sha="abc1234",
            entries=[],
        )

    assert sha is None


@respx.mock
async def test_replay_for_revision_happy_path(git_mock: test_utils.GitMock) -> None:
    """End-to-end: merge-tree → diff → upload → returns server commit SHA."""
    base_url = "https://api.github.com"
    respx.post(f"{base_url}/repos/owner/repo/git/trees").mock(
        return_value=httpx.Response(201, json={"sha": "server_tree_sha"}),
    )
    respx.post(f"{base_url}/repos/owner/repo/git/commits").mock(
        return_value=httpx.Response(201, json={"sha": "server_commit_sha"}),
    )
    git_mock.mock("rev-parse", "old_sha^", output="parent_old_sha")
    git_mock.mock("rev-parse", "new_sha^", output="parent_new_sha")
    git_mock.mock(
        "merge-tree",
        "--write-tree",
        "--merge-base=parent_old_sha",
        "parent_new_sha",
        "old_sha",
        output="merged_tree_sha",
    )
    git_mock.mock("rev-parse", "parent_new_sha^{tree}", output="parent_new_tree_sha")
    git_mock.mock(
        "diff-tree",
        "-r",
        "--raw",
        "--no-renames",
        "parent_new_tree_sha",
        "merged_tree_sha",
        output=":100644 100644 aaa bbb M\tsrc/x.py\n",
    )

    async with httpx.AsyncClient(base_url=base_url) as client:
        sha = await replay.replay_for_revision(
            client=client,
            user="owner",
            repo="repo",
            old_sha="old_sha",
            new_sha="new_sha",
        )

    assert sha == "server_commit_sha"


@respx.mock
async def test_replay_for_revision_conflict_returns_none(
    git_mock: test_utils.GitMock,
) -> None:
    """A merge-tree conflict short-circuits to None (no API calls)."""
    git_mock.mock("rev-parse", "old_sha^", output="parent_old_sha")
    git_mock.mock("rev-parse", "new_sha^", output="parent_new_sha")
    git_mock.mock_error(
        "merge-tree",
        "--write-tree",
        "--merge-base=parent_old_sha",
        "parent_new_sha",
        "old_sha",
    )

    async with httpx.AsyncClient(base_url="https://api.github.com") as client:
        sha = await replay.replay_for_revision(
            client=client,
            user="owner",
            repo="repo",
            old_sha="old_sha",
            new_sha="new_sha",
        )

    assert sha is None


@respx.mock
async def test_replay_for_revision_no_diff_returns_none(
    git_mock: test_utils.GitMock,
) -> None:
    """If merged tree equals parent_new's tree, there's nothing to upload."""
    git_mock.mock("rev-parse", "old_sha^", output="parent_old_sha")
    git_mock.mock("rev-parse", "new_sha^", output="parent_new_sha")
    git_mock.mock(
        "merge-tree",
        "--write-tree",
        "--merge-base=parent_old_sha",
        "parent_new_sha",
        "old_sha",
        output="merged_tree_sha",
    )
    git_mock.mock("rev-parse", "parent_new_sha^{tree}", output="parent_new_tree_sha")
    git_mock.mock(
        "diff-tree",
        "-r",
        "--raw",
        "--no-renames",
        "parent_new_tree_sha",
        "merged_tree_sha",
        output="",
    )

    async with httpx.AsyncClient(base_url="https://api.github.com") as client:
        sha = await replay.replay_for_revision(
            client=client,
            user="owner",
            repo="repo",
            old_sha="old_sha",
            new_sha="new_sha",
        )

    assert sha is None


@respx.mock
async def test_replay_for_revision_rev_parse_tree_error_returns_none(
    git_mock: test_utils.GitMock,
) -> None:
    """If rev-parse <commit>^{tree} fails, replay_for_revision returns None."""
    git_mock.mock("rev-parse", "old_sha^", output="parent_old_sha")
    git_mock.mock("rev-parse", "new_sha^", output="parent_new_sha")
    git_mock.mock(
        "merge-tree",
        "--write-tree",
        "--merge-base=parent_old_sha",
        "parent_new_sha",
        "old_sha",
        output="merged_tree_sha",
    )
    git_mock.mock_error("rev-parse", "parent_new_sha^{tree}")

    async with httpx.AsyncClient(base_url="https://api.github.com") as client:
        sha = await replay.replay_for_revision(
            client=client,
            user="owner",
            repo="repo",
            old_sha="old_sha",
            new_sha="new_sha",
        )

    assert sha is None
