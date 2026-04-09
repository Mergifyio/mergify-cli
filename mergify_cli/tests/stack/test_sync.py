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

import subprocess
import sys
from typing import TYPE_CHECKING

import pytest

from mergify_cli.stack import sync as stack_sync_mod
from mergify_cli.tests import utils as test_utils


if TYPE_CHECKING:
    import pathlib

    import respx


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX shell not available on Windows")
def test_write_drop_script_produces_working_script(tmp_path: pathlib.Path) -> None:
    """Test that the generated drop script correctly modifies a rebase todo file."""
    merged_shas = {"abc1234567890abcdef1234567890abcdef123456"}

    script_path = stack_sync_mod._write_drop_script(merged_shas)
    try:
        # Create a fake rebase todo file
        todo = tmp_path / "git-rebase-todo"
        todo.write_text(
            "pick abc1234 First commit\n"
            "pick def5678 Second commit\n"
            "pick ghi9012 Third commit\n",
        )

        # Run the script
        subprocess.run(
            [str(script_path), str(todo)],
            check=True,
        )

        result = todo.read_text()
        assert "drop abc1234 First commit\n" in result
        assert "pick def5678 Second commit\n" in result
        assert "pick ghi9012 Third commit\n" in result
    finally:
        script_path.unlink(missing_ok=True)


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX shell not available on Windows")
def test_write_drop_script_multiple_shas(tmp_path: pathlib.Path) -> None:
    """Test drop script with multiple merged commits."""
    merged_shas = {
        "abc1234567890abcdef1234567890abcdef123456",
        "ghi9012567890abcdef1234567890abcdef123456",
    }

    script_path = stack_sync_mod._write_drop_script(merged_shas)
    try:
        todo = tmp_path / "git-rebase-todo"
        todo.write_text(
            "pick abc1234 First commit\n"
            "pick def5678 Second commit\n"
            "pick ghi9012 Third commit\n",
        )

        subprocess.run(
            [str(script_path), str(todo)],
            check=True,
        )

        result = todo.read_text()
        assert "drop abc1234 First commit\n" in result
        assert "pick def5678 Second commit\n" in result
        assert "drop ghi9012 Third commit\n" in result
    finally:
        script_path.unlink(missing_ok=True)


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_sync_detects_merged_commits(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
) -> None:
    """Test that get_sync_status correctly classifies merged vs remaining commits."""
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")

    git_mock.commit(
        test_utils.Commit(
            sha="commit1_sha",
            title="First commit",
            message="Message commit 1",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf50",
        ),
    )
    git_mock.commit(
        test_utils.Commit(
            sha="commit2_sha",
            title="Second commit",
            message="Message commit 2",
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
                        "url": "https://api.github.com/repos/user/repo/pulls/1",
                    },
                },
                {
                    "pull_request": {
                        "url": "https://api.github.com/repos/user/repo/pulls/2",
                    },
                },
            ],
        },
    )
    # commit1: merged
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
            "merge_commit_sha": "merge_sha_1",
            "draft": False,
            "node_id": "",
        },
    )
    # commit2: open
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
            "draft": False,
            "node_id": "",
        },
    )

    result = await stack_sync_mod.get_sync_status(
        github_server="https://api.github.com/",
        token="",
        trunk=("origin", "main"),
    )

    assert len(result.merged) == 1
    assert result.merged[0].title == "First commit"
    assert result.merged[0].commit_sha == "commit1_sha"

    assert len(result.remaining) == 1
    assert result.remaining[0].title == "Second commit"


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_sync_up_to_date(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
) -> None:
    """Test that get_sync_status reports up_to_date when no commits are merged."""
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")

    git_mock.commit(
        test_utils.Commit(
            sha="commit1_sha",
            title="First commit",
            message="Message commit 1",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf60",
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
                        "url": "https://api.github.com/repos/user/repo/pulls/1",
                    },
                },
            ],
        },
    )
    # commit1: open (not merged)
    respx_mock.get("/repos/user/repo/pulls/1").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/1",
            "number": "1",
            "title": "First commit",
            "head": {
                "sha": "commit1_sha",
                "ref": "current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf60",
            },
            "state": "open",
            "merged_at": None,
            "draft": False,
            "node_id": "",
        },
    )

    result = await stack_sync_mod.get_sync_status(
        github_server="https://api.github.com/",
        token="",
        trunk=("origin", "main"),
    )

    assert result.up_to_date is True
    assert len(result.merged) == 0
    assert len(result.remaining) == 1


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_sync_all_merged(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
) -> None:
    """Test that get_sync_status reports all_merged when every commit is merged."""
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")

    git_mock.commit(
        test_utils.Commit(
            sha="commit1_sha",
            title="First commit",
            message="Message commit 1",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf70",
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
                        "url": "https://api.github.com/repos/user/repo/pulls/1",
                    },
                },
            ],
        },
    )
    # commit1: merged
    respx_mock.get("/repos/user/repo/pulls/1").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/1",
            "number": "1",
            "title": "First commit",
            "head": {
                "sha": "commit1_sha",
                "ref": "current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf70",
            },
            "state": "closed",
            "merged_at": "2024-01-01T00:00:00Z",
            "merge_commit_sha": "merge_sha",
            "draft": False,
            "node_id": "",
        },
    )

    result = await stack_sync_mod.get_sync_status(
        github_server="https://api.github.com/",
        token="",
        trunk=("origin", "main"),
    )

    assert result.all_merged is True
    assert len(result.merged) == 1
    assert len(result.remaining) == 0


# --- smart_rebase() direct tests ---


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_smart_rebase_no_merged_uses_pull_rebase(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
) -> None:
    """smart_rebase with no merged commits falls back to git pull --rebase."""
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")
    git_mock.mock("pull", "--rebase", "origin", "main", output="")

    git_mock.commit(
        test_utils.Commit(
            sha="commit1_sha",
            title="Open commit",
            message="Message 1",
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
                        "url": "https://api.github.com/repos/user/repo/pulls/1",
                    },
                },
            ],
        },
    )
    respx_mock.get("/repos/user/repo/pulls/1").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/1",
            "number": "1",
            "title": "Open commit",
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

    status = await stack_sync_mod.smart_rebase(
        github_server="https://api.github.com/",
        token="",
        trunk=("origin", "main"),
    )

    assert status.up_to_date
    assert git_mock.has_been_called_with("pull", "--rebase", "origin", "main")


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_smart_rebase_with_merged_uses_rebase_i(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
) -> None:
    """smart_rebase with merged commits uses git rebase -i to drop them."""
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")
    git_mock.mock("rebase", "-i", "origin/main", output="")

    git_mock.commit(
        test_utils.Commit(
            sha="commit1_sha",
            title="Merged commit",
            message="Message 1",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf50",
        ),
    )
    git_mock.commit(
        test_utils.Commit(
            sha="commit2_sha",
            title="Open commit",
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
                        "url": "https://api.github.com/repos/user/repo/pulls/1",
                    },
                },
                {
                    "pull_request": {
                        "url": "https://api.github.com/repos/user/repo/pulls/2",
                    },
                },
            ],
        },
    )
    respx_mock.get("/repos/user/repo/pulls/1").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/1",
            "number": "1",
            "title": "Merged commit",
            "head": {
                "sha": "commit1_sha",
                "ref": "current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf50",
            },
            "state": "closed",
            "merged_at": "2024-01-01T00:00:00Z",
            "merge_commit_sha": "merge_sha_1",
            "draft": False,
            "node_id": "",
        },
    )
    respx_mock.get("/repos/user/repo/pulls/2").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/2",
            "number": "2",
            "title": "Open commit",
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

    status = await stack_sync_mod.smart_rebase(
        github_server="https://api.github.com/",
        token="",
        trunk=("origin", "main"),
    )

    assert len(status.merged) == 1
    assert len(status.remaining) == 1
    assert git_mock.has_been_called_with("rebase", "-i", "origin/main")
    assert not git_mock.has_been_called_with("pull", "--rebase", "origin", "main")


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_smart_rebase_all_merged_uses_pull_rebase(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
) -> None:
    """smart_rebase with all commits merged falls back to git pull --rebase."""
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")
    git_mock.mock("pull", "--rebase", "origin", "main", output="")

    git_mock.commit(
        test_utils.Commit(
            sha="commit1_sha",
            title="Merged commit",
            message="Message 1",
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
                        "url": "https://api.github.com/repos/user/repo/pulls/1",
                    },
                },
            ],
        },
    )
    respx_mock.get("/repos/user/repo/pulls/1").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/1",
            "number": "1",
            "title": "Merged commit",
            "head": {
                "sha": "commit1_sha",
                "ref": "current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf50",
            },
            "state": "closed",
            "merged_at": "2024-01-01T00:00:00Z",
            "merge_commit_sha": "merge_sha",
            "draft": False,
            "node_id": "",
        },
    )

    status = await stack_sync_mod.smart_rebase(
        github_server="https://api.github.com/",
        token="",
        trunk=("origin", "main"),
    )

    assert status.all_merged
    assert git_mock.has_been_called_with("pull", "--rebase", "origin", "main")


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_smart_rebase_multiple_merged(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
) -> None:
    """smart_rebase drops multiple merged commits in one rebase."""
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")
    git_mock.mock("rebase", "-i", "origin/main", output="")

    git_mock.commit(
        test_utils.Commit(
            sha="commit1_sha",
            title="First merged",
            message="Message 1",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf50",
        ),
    )
    git_mock.commit(
        test_utils.Commit(
            sha="commit2_sha",
            title="Second merged",
            message="Message 2",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf51",
        ),
    )
    git_mock.commit(
        test_utils.Commit(
            sha="commit3_sha",
            title="Open commit",
            message="Message 3",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf52",
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
    respx_mock.get("/repos/user/repo/pulls/1").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/1",
            "number": "1",
            "title": "First merged",
            "head": {
                "sha": "commit1_sha",
                "ref": "current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf50",
            },
            "state": "closed",
            "merged_at": "2024-01-01T00:00:00Z",
            "merge_commit_sha": "merge1",
            "draft": False,
            "node_id": "",
        },
    )
    respx_mock.get("/repos/user/repo/pulls/2").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/2",
            "number": "2",
            "title": "Second merged",
            "head": {
                "sha": "commit2_sha",
                "ref": "current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf51",
            },
            "state": "closed",
            "merged_at": "2024-01-02T00:00:00Z",
            "merge_commit_sha": "merge2",
            "draft": False,
            "node_id": "",
        },
    )
    respx_mock.get("/repos/user/repo/pulls/3").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/3",
            "number": "3",
            "title": "Open commit",
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

    status = await stack_sync_mod.smart_rebase(
        github_server="https://api.github.com/",
        token="",
        trunk=("origin", "main"),
    )

    assert len(status.merged) == 2
    assert len(status.remaining) == 1
    assert status.merged[0].title == "First merged"
    assert status.merged[1].title == "Second merged"
    assert git_mock.has_been_called_with("rebase", "-i", "origin/main")


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_smart_rebase_mid_stack_merged(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
) -> None:
    """smart_rebase handles interleaved merged/open commits (A open, B merged, C open)."""
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")
    git_mock.mock("rebase", "-i", "origin/main", output="")

    git_mock.commit(
        test_utils.Commit(
            sha="commit1_sha",
            title="First open",
            message="Message 1",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf50",
        ),
    )
    git_mock.commit(
        test_utils.Commit(
            sha="commit2_sha",
            title="Mid-stack merged",
            message="Message 2",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf51",
        ),
    )
    git_mock.commit(
        test_utils.Commit(
            sha="commit3_sha",
            title="Last open",
            message="Message 3",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf52",
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
    respx_mock.get("/repos/user/repo/pulls/1").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/1",
            "number": "1",
            "title": "First open",
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
    respx_mock.get("/repos/user/repo/pulls/2").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/2",
            "number": "2",
            "title": "Mid-stack merged",
            "head": {
                "sha": "commit2_sha",
                "ref": "current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf51",
            },
            "state": "closed",
            "merged_at": "2024-01-01T00:00:00Z",
            "merge_commit_sha": "merge_sha",
            "draft": False,
            "node_id": "",
        },
    )
    respx_mock.get("/repos/user/repo/pulls/3").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/3",
            "number": "3",
            "title": "Last open",
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

    status = await stack_sync_mod.smart_rebase(
        github_server="https://api.github.com/",
        token="",
        trunk=("origin", "main"),
    )

    assert len(status.merged) == 1
    assert status.merged[0].title == "Mid-stack merged"
    assert len(status.remaining) == 2
    assert status.remaining[0].title == "First open"
    assert status.remaining[1].title == "Last open"
    assert git_mock.has_been_called_with("rebase", "-i", "origin/main")


# --- stack_sync() integration tests ---


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_sync_up_to_date_after_rebase(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Test: no merged commits — rebase onto trunk, report up to date."""
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")
    git_mock.mock("fetch", "origin", "main", output="")
    git_mock.mock("pull", "--rebase", "origin", "main", output="")

    git_mock.commit(
        test_utils.Commit(
            sha="commit1_sha",
            title="Open commit",
            message="Message 1",
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
                        "url": "https://api.github.com/repos/user/repo/pulls/1",
                    },
                },
            ],
        },
    )
    respx_mock.get("/repos/user/repo/pulls/1").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/1",
            "number": "1",
            "title": "Open commit",
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

    await stack_sync_mod.stack_sync(
        github_server="https://api.github.com/",
        token="",
        trunk=("origin", "main"),
    )

    captured = capsys.readouterr()
    assert "up to date" in captured.out
    assert git_mock.has_been_called_with("fetch", "origin", "main")
    assert git_mock.has_been_called_with("pull", "--rebase", "origin", "main")


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_sync_drops_merged_and_rebases_in_one_operation(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Test: merged commit detected, dropped and rebased in single git rebase -i.

    When sync finds merged commits, it does a single `git rebase -i origin/main`
    with a drop script — this both removes the merged commit and rebases onto
    trunk in one operation, avoiding conflicts from trying to reapply a commit
    whose content was modified on GitHub before merge.
    """
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")
    git_mock.mock("fetch", "origin", "main", output="")
    # Single rebase onto origin/main (not fork-point)
    git_mock.mock("rebase", "-i", "origin/main", output="")

    git_mock.commit(
        test_utils.Commit(
            sha="commit1_sha",
            title="Modified on GitHub then merged",
            message="Message 1",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf50",
        ),
    )
    git_mock.commit(
        test_utils.Commit(
            sha="commit2_sha",
            title="Open commit",
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
                        "url": "https://api.github.com/repos/user/repo/pulls/1",
                    },
                },
                {
                    "pull_request": {
                        "url": "https://api.github.com/repos/user/repo/pulls/2",
                    },
                },
            ],
        },
    )
    # commit1: merged (modified on GitHub — would conflict with git pull --rebase)
    respx_mock.get("/repos/user/repo/pulls/1").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/1",
            "number": "1",
            "title": "Modified on GitHub then merged",
            "head": {
                "sha": "commit1_sha",
                "ref": "current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf50",
            },
            "state": "closed",
            "merged_at": "2024-01-01T00:00:00Z",
            "merge_commit_sha": "merge_sha_1",
            "draft": False,
            "node_id": "",
        },
    )
    # commit2: open
    respx_mock.get("/repos/user/repo/pulls/2").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/2",
            "number": "2",
            "title": "Open commit",
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

    await stack_sync_mod.stack_sync(
        github_server="https://api.github.com/",
        token="",
        trunk=("origin", "main"),
    )

    captured = capsys.readouterr()
    assert "Modified on GitHub then merged" in captured.out
    assert "Dropped 1 merged" in captured.out
    # Should use single rebase -i onto origin/main (not git pull --rebase)
    assert git_mock.has_been_called_with("rebase", "-i", "origin/main")
    assert not git_mock.has_been_called_with("pull", "--rebase", "origin", "main")


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_sync_all_merged_suggests_checkout(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Test: all commits merged — suggests switching to main."""
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")
    git_mock.mock("fetch", "origin", "main", output="")
    git_mock.mock("pull", "--rebase", "origin", "main", output="")

    git_mock.commit(
        test_utils.Commit(
            sha="commit1_sha",
            title="Only commit",
            message="Message 1",
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
                        "url": "https://api.github.com/repos/user/repo/pulls/1",
                    },
                },
            ],
        },
    )
    respx_mock.get("/repos/user/repo/pulls/1").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/1",
            "number": "1",
            "title": "Only commit",
            "head": {
                "sha": "commit1_sha",
                "ref": "current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf50",
            },
            "state": "closed",
            "merged_at": "2024-01-01T00:00:00Z",
            "merge_commit_sha": "merge_sha",
            "draft": False,
            "node_id": "",
        },
    )

    await stack_sync_mod.stack_sync(
        github_server="https://api.github.com/",
        token="",
        trunk=("origin", "main"),
    )

    captured = capsys.readouterr()
    assert "All commits" in captured.out
    assert "git checkout main" in captured.out


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_sync_dry_run(
    git_mock: test_utils.GitMock,
    respx_mock: respx.MockRouter,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Test: dry-run shows what would happen without fetching or rebasing."""
    git_mock.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")

    git_mock.commit(
        test_utils.Commit(
            sha="commit1_sha",
            title="Merged commit",
            message="Message 1",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf80",
        ),
    )
    git_mock.commit(
        test_utils.Commit(
            sha="commit2_sha",
            title="Open commit",
            message="Message 2",
            change_id="I29617d37762fd69809c255d7e7073cb11f8fbf81",
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
                        "url": "https://api.github.com/repos/user/repo/pulls/1",
                    },
                },
                {
                    "pull_request": {
                        "url": "https://api.github.com/repos/user/repo/pulls/2",
                    },
                },
            ],
        },
    )
    respx_mock.get("/repos/user/repo/pulls/1").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/1",
            "number": "1",
            "title": "Merged commit",
            "head": {
                "sha": "commit1_sha",
                "ref": "current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf80",
            },
            "state": "closed",
            "merged_at": "2024-01-01T00:00:00Z",
            "merge_commit_sha": "merge_sha_1",
            "draft": False,
            "node_id": "",
        },
    )
    respx_mock.get("/repos/user/repo/pulls/2").respond(
        200,
        json={
            "html_url": "https://github.com/user/repo/pull/2",
            "number": "2",
            "title": "Open commit",
            "head": {
                "sha": "commit2_sha",
                "ref": "current-branch/I29617d37762fd69809c255d7e7073cb11f8fbf81",
            },
            "state": "open",
            "merged_at": None,
            "draft": False,
            "node_id": "",
        },
    )

    await stack_sync_mod.stack_sync(
        github_server="https://api.github.com/",
        token="",
        trunk=("origin", "main"),
        dry_run=True,
    )

    captured = capsys.readouterr()
    assert "Merged commit" in captured.out
    assert "1 commit(s) would remain" in captured.out
    # No fetch or rebase in dry-run
    assert not git_mock.has_been_called_with("fetch", "origin", "main")
    assert not git_mock.has_been_called_with("pull", "--rebase", "origin", "main")
