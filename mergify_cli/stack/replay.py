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

import dataclasses

from mergify_cli import utils


@dataclasses.dataclass(frozen=True)
class MergedTree:
    tree_sha: str
    parent_new_sha: str


async def compute_merged_tree(
    *,
    old_sha: str,
    new_sha: str,
) -> MergedTree | None:
    """Compute the tree of `old_sha` replayed onto `parent(new_sha)`.

    Returns None on conflict, missing parents, or any git error.
    Requires git >= 2.38 for `git merge-tree --write-tree`.
    """
    try:
        parent_old_sha = await utils.git("rev-parse", f"{old_sha}^")
        parent_new_sha = await utils.git("rev-parse", f"{new_sha}^")
    except utils.CommandError:
        return None

    try:
        output = await utils.git(
            "merge-tree",
            "--write-tree",
            f"--merge-base={parent_old_sha}",
            parent_new_sha,
            old_sha,
        )
    except utils.CommandError:
        # Non-zero exit = conflict (or older git that doesn't support flags).
        return None

    # On a clean merge, the first line of stdout is the tree SHA.
    lines = output.splitlines()
    if not lines:
        return None

    return MergedTree(tree_sha=lines[0], parent_new_sha=parent_new_sha)


def _mode_to_type(mode: str) -> str:
    """Map a git tree-entry mode to the GitHub Git Data API type field."""
    if mode == "160000":
        return "commit"
    if mode == "040000":
        return "tree"
    return "blob"


async def compute_tree_delta(
    *,
    base_tree_sha: str,
    merged_tree_sha: str,
) -> list[dict[str, str | None]]:
    """Return Git Data API tree entries for everything that differs.

    Output format matches `POST /repos/{owner}/{repo}/git/trees` entry
    schema: each item has path, mode, type, and sha. For deletions, sha
    is None to instruct GitHub to remove the path from base_tree.
    """
    output = await utils.git(
        "diff-tree",
        "-r",
        "--raw",
        "--no-renames",
        base_tree_sha,
        merged_tree_sha,
    )
    entries: list[dict[str, str | None]] = []
    for line in output.splitlines():
        if not line.startswith(":"):
            continue
        # Format: ":mode_old mode_new sha_old sha_new STATUS\tpath"
        # STATUS is one of M (modified), A (added), D (deleted), T (type-changed).
        meta, _, path = line.partition("\t")
        if not path:
            continue
        parts = meta.lstrip(":").split()
        if len(parts) < 5:
            continue
        mode_old, mode_new, _sha_old, sha_new, status = parts[:5]
        if status == "D":
            # Deletion: GitHub API expects sha=null with the path.
            entries.append(
                {
                    "path": path,
                    "mode": mode_old,
                    "type": _mode_to_type(mode_old),
                    "sha": None,
                },
            )
        else:
            entries.append(
                {
                    "path": path,
                    "mode": mode_new,
                    "type": _mode_to_type(mode_new),
                    "sha": sha_new,
                },
            )
    return entries
