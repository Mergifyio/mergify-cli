from __future__ import annotations

import subprocess

import click


def _run(cmd: list[str]) -> str:
    try:
        return subprocess.check_output(cmd, text=True, encoding="utf-8").strip()
    except subprocess.CalledProcessError as e:
        msg = f"Command failed: {' '.join(cmd)}\n{e}"
        raise click.ClickException(msg) from e


def git_changed_files(base: str) -> list[str]:
    # Committed changes only between base_sha and HEAD.
    # Includes: Added (A), Copied (C), Modified (M), Renamed (R), Type-changed (T), Deleted (D)
    # Excludes: Unmerged (U), Unknown (X), Broken (B)
    out = _run(
        ["git", "diff", "--name-only", "--diff-filter=ACMRTD", f"{base}...HEAD"],
    )
    return [line for line in out.splitlines() if line]
