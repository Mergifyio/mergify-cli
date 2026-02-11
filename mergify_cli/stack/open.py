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

"""Open stack PRs in browser."""

from __future__ import annotations

import sys
import typing
import webbrowser

import questionary

from mergify_cli import console
from mergify_cli import utils
from mergify_cli.stack.list import get_stack_list


if typing.TYPE_CHECKING:
    from mergify_cli.stack.list import StackListEntry


def _build_choices(entries: list[StackListEntry]) -> list[questionary.Choice]:
    """Build questionary choices from stack entries."""
    choices = []
    for entry in entries:
        short_sha = entry.commit_sha[:7]
        if entry.pull_number is not None:
            label = f"#{entry.pull_number} {entry.title} ({short_sha})"
        else:
            label = f"(no PR) {entry.title} ({short_sha})"
        choices.append(questionary.Choice(title=label, value=entry))
    return choices


async def stack_open(
    github_server: str,
    token: str,
    *,
    commit: str | None = None,
) -> None:
    """Open PR for the specified commit in browser.

    Args:
        github_server: GitHub API server URL
        token: GitHub personal access token
        commit: Commit reference (SHA, HEAD, HEAD~1, etc.). If None, shows interactive picker.
    """
    trunk_str = await utils.get_trunk()
    trunk_parts = trunk_str.split("/", maxsplit=1)
    trunk = (trunk_parts[0], trunk_parts[1])

    output = await get_stack_list(
        github_server=github_server,
        token=token,
        trunk=trunk,
    )

    if not output.entries:
        console.print("[yellow]No commits in stack[/]")
        sys.exit(1)

    entry: StackListEntry

    # Interactive selection if no commit specified
    if commit is None:
        choices = _build_choices(output.entries)

        selected = await questionary.select(
            "Select a PR to open:",
            choices=choices,
            default=choices[-1],  # Default to HEAD (last entry)
        ).ask_async()

        if selected is None:
            # User cancelled (Ctrl+C)
            sys.exit(0)

        entry = selected
    else:
        # Resolve commit ref to full SHA
        try:
            commit_sha = await utils.git("rev-parse", commit)
        except utils.CommandError:
            console.print(f"[red]Commit `{commit}` not found[/]")
            sys.exit(1)

        # Find entry matching the commit SHA
        found_entry = next(
            (e for e in output.entries if e.commit_sha == commit_sha),
            None,
        )

        if found_entry is None:
            console.print(
                f"[red]Commit `{commit}` ({commit_sha[:7]}) not found in stack[/]",
            )
            sys.exit(1)

        entry = found_entry

    if entry.pull_url is None:
        console.print(
            f"[yellow]No PR for: {entry.title} ({entry.commit_sha[:7]})[/]",
        )
        console.print("Run `mergify stack push` first.")
        sys.exit(1)

    console.print(f"Opening PR #{entry.pull_number}: {entry.title}")
    webbrowser.open(entry.pull_url)
