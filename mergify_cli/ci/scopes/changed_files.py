from __future__ import annotations

import subprocess
import sys

from mergify_cli.ci.scopes import exceptions


COMMITS_BATCH_SIZE = 100


class ChangedFilesError(exceptions.ScopesError):
    pass


def _run(cmd: list[str]) -> str:
    return subprocess.check_output(cmd, text=True, encoding="utf-8").strip()


def has_merge_base(base: str, head: str) -> bool:
    try:
        _run(["git", "merge-base", base, head])
    except subprocess.CalledProcessError:
        return False
    return True


def get_commits_count() -> int:
    return int(_run(["git", "rev-list", "--count", "--all"]))


def ensure_git_history(base: str, head: str) -> None:
    fetch_depth = COMMITS_BATCH_SIZE

    if not has_merge_base(base, head):
        _run(
            [
                "git",
                "fetch",
                "--no-tags",
                f"--depth={fetch_depth}",
                "origin",
                base,
                head,
            ],
        )

    last_commits_count = get_commits_count()
    while not has_merge_base(base, head):
        fetch_depth = min(fetch_depth * 2, sys.maxsize)
        _run(["git", "fetch", f"--deepen={fetch_depth}", "origin", base, "HEAD"])
        commits_count = get_commits_count()
        if commits_count == last_commits_count:
            if not has_merge_base(base, head):
                msg = f"Cannot find a common ancestor between {base} and {head}"
                raise ChangedFilesError(msg)

            break
        last_commits_count = commits_count


def git_changed_files(base: str) -> list[str]:
    head = "HEAD"
    ensure_git_history(base, head)
    # Committed changes only between base_sha and HEAD.
    # Includes: Added (A), Copied (C), Modified (M), Renamed (R), Type-changed (T), Deleted (D)
    # Excludes: Unmerged (U), Unknown (X), Broken (B)
    try:
        out = _run(
            ["git", "diff", "--name-only", "--diff-filter=ACMRTD", f"{base}...{head}"],
        )
    except subprocess.CalledProcessError as e:
        msg = f"Command failed: {' '.join(e.cmd)}\n{e}"
        raise ChangedFilesError(msg)

    return [line for line in out.splitlines() if line]
