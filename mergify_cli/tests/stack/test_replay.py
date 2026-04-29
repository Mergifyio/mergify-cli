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

from unittest import mock

from mergify_cli import utils
from mergify_cli.stack import replay


async def test_compute_merged_tree_clean() -> None:
    """Clean merge returns the merged tree SHA."""

    async def fake_git(*args: str) -> str:
        if args[0] == "rev-parse" and args[1] == "old_sha^":
            return "parent_old_sha"
        if args[0] == "rev-parse" and args[1] == "new_sha^":
            return "parent_new_sha"
        if args == (
            "merge-tree",
            "--write-tree",
            "--merge-base=parent_old_sha",
            "parent_new_sha",
            "old_sha",
        ):
            # On clean merge, git prints the tree SHA on the first line.
            return "merged_tree_sha"
        msg = f"unexpected git call: {args}"
        raise AssertionError(msg)

    with mock.patch.object(utils, "git", side_effect=fake_git):
        result = await replay.compute_merged_tree(
            old_sha="old_sha",
            new_sha="new_sha",
        )
    assert result == replay.MergedTree(
        tree_sha="merged_tree_sha",
        parent_new_sha="parent_new_sha",
    )


async def test_compute_merged_tree_conflict_returns_none() -> None:
    """Conflicting merge returns None."""

    async def fake_git(*args: str) -> str:
        if args[0] == "rev-parse" and args[1] == "old_sha^":
            return "parent_old_sha"
        if args[0] == "rev-parse" and args[1] == "new_sha^":
            return "parent_new_sha"
        if args[0] == "merge-tree":
            raise utils.CommandError(("git", "merge-tree"), 1, b"CONFLICT (content)")
        msg = f"unexpected git call: {args}"
        raise AssertionError(msg)

    with mock.patch.object(utils, "git", side_effect=fake_git):
        result = await replay.compute_merged_tree(
            old_sha="old_sha",
            new_sha="new_sha",
        )
    assert result is None


async def test_compute_merged_tree_rev_parse_error_returns_none() -> None:
    """If `git rev-parse` fails (e.g., parent not fetched), return None."""

    async def fake_git(*args: str) -> str:
        if args[0] == "rev-parse":
            raise utils.CommandError(("git", "rev-parse"), 128, b"unknown revision")
        msg = f"unexpected git call: {args}"
        raise AssertionError(msg)

    with mock.patch.object(utils, "git", side_effect=fake_git):
        result = await replay.compute_merged_tree(
            old_sha="old_sha",
            new_sha="new_sha",
        )
    assert result is None


async def test_compute_tree_delta_parses_modifications_and_deletions() -> None:
    """diff-tree --raw output is converted into Git Data API tree entries."""
    raw_output = (
        ":100644 100644 aaa1111 bbb2222 M\tsrc/a.py\n"
        ":100755 000000 ccc3333 0000000 D\tscripts/exec.sh\n"
        ":000000 100755 0000000 ddd4444 A\tscripts/run.sh\n"
        ":100644 100755 eee5555 fff6666 T\tsrc/c.py\n"
    )

    async def fake_git(*args: str) -> str:
        assert args == (
            "diff-tree",
            "-r",
            "--raw",
            "--no-renames",
            "base_tree_sha",
            "merged_tree_sha",
        )
        return raw_output

    with mock.patch.object(utils, "git", side_effect=fake_git):
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


async def test_compute_tree_delta_empty_when_no_diff() -> None:
    """Empty diff-tree output produces an empty entry list."""

    async def fake_git(*_args: str) -> str:
        return ""

    with mock.patch.object(utils, "git", side_effect=fake_git):
        entries = await replay.compute_tree_delta(
            base_tree_sha="x",
            merged_tree_sha="y",
        )
    assert entries == []
