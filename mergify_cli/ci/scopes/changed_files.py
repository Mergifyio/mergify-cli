from __future__ import annotations

import re
import subprocess
import sys

from mergify_cli.ci.scopes import exceptions


COMMITS_BATCH_SIZE = 100

# Scoped namespace for refs we fetch ourselves, to avoid clashing with
# refs/remotes/origin/* (which may not exist or may point elsewhere).
FETCHED_REF_PREFIX = "refs/mergify-cli/fetched/"

# Only full 40-char SHAs — abbreviated SHAs would false-match branch names
# like "deadbeef" and cause them to be fetched without a refspec.
_SHA_RE = re.compile(r"^[0-9a-f]{40}$")


class ChangedFilesError(exceptions.ScopesError):
    pass


def _run(cmd: list[str]) -> str:
    return subprocess.check_output(cmd, text=True, encoding="utf-8").strip()  # noqa: S603


def _is_sha(ref: str) -> bool:
    return bool(_SHA_RE.match(ref))


def _is_local_ref(ref: str) -> bool:
    return ref == "HEAD" or ref.startswith(("HEAD~", "HEAD^"))


def _local_ref(ref: str) -> str:
    if _is_sha(ref) or _is_local_ref(ref):
        return ref
    return f"{FETCHED_REF_PREFIX}{ref}"


def _fetch_arg(ref: str) -> str | None:
    if _is_local_ref(ref):
        return None
    if _is_sha(ref):
        return ref
    # `git fetch origin <branch>` only updates FETCH_HEAD; use an explicit
    # refspec so the branch becomes a real local ref we can name later.
    return f"+{ref}:{_local_ref(ref)}"


def has_merge_base(base: str, head: str) -> bool:
    try:
        _run(["git", "merge-base", "--", base, head])
    except subprocess.CalledProcessError:
        return False
    return True


def get_commits_count() -> int:
    return int(_run(["git", "rev-list", "--count", "--all"]))


def _fetch_cmd(depth_flag: str, fetch_args: list[str]) -> list[str]:
    cmd = ["git", "fetch", "--no-tags", depth_flag, "origin"]
    if fetch_args:
        cmd.append("--")
        cmd.extend(fetch_args)
    return cmd


def ensure_git_history(base: str, head: str) -> tuple[str, str]:
    if has_merge_base(base, head):
        return base, head

    fetch_args = [a for a in (_fetch_arg(base), _fetch_arg(head)) if a]
    local_base = _local_ref(base)
    local_head = _local_ref(head)
    fetch_depth = COMMITS_BATCH_SIZE

    _run(_fetch_cmd(f"--depth={fetch_depth}", fetch_args))

    last_commits_count = get_commits_count()
    while not has_merge_base(local_base, local_head):
        fetch_depth = min(fetch_depth * 2, sys.maxsize)
        _run(_fetch_cmd(f"--deepen={fetch_depth}", fetch_args))
        commits_count = get_commits_count()
        if commits_count == last_commits_count:
            if not has_merge_base(local_base, local_head):
                msg = f"Cannot find a common ancestor between {base} and {head}"
                raise ChangedFilesError(msg)

            break
        last_commits_count = commits_count

    return local_base, local_head


def git_changed_files(base: str, head: str) -> list[str]:
    local_base, local_head = ensure_git_history(base, head)
    # Committed changes only between base_sha and HEAD.
    # Includes: Added (A), Copied (C), Modified (M), Renamed (R), Type-changed (T), Deleted (D)
    # Excludes: Unmerged (U), Unknown (X), Broken (B)
    try:
        out = _run(
            [
                "git",
                "diff",
                "--name-only",
                "--diff-filter=ACMRTD",
                f"{local_base}...{local_head}",
                "--",
            ],
        )
    except subprocess.CalledProcessError as e:
        msg = f"Command failed: {' '.join(e.cmd)}\n{e}"
        raise ChangedFilesError(msg)

    return [line for line in out.splitlines() if line]
