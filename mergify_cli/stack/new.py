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

import sys

from mergify_cli import console
from mergify_cli import utils


async def stack_new(
    *,
    name: str,
    base: tuple[str, str] | None,
    checkout: bool,
) -> None:
    """Create a new stack branch.

    Args:
        name: The name of the new branch.
        base: The base branch as (remote, branch) tuple, or None for default trunk.
        checkout: Whether to checkout the new branch after creation.
    """
    # Determine base branch
    if base is None:
        try:
            trunk = await utils.get_trunk()
        except utils.CommandError:
            console.print(
                "[red]Could not determine trunk branch. "
                "Please set upstream tracking or use --base to specify the base branch.[/]",
            )
            sys.exit(1)
        else:
            remote, base_branch = trunk.split("/", maxsplit=1)
    else:
        remote, base_branch = base

    # Fetch latest from remote
    with console.status(f"Fetching latest from {remote}..."):
        try:
            await utils.git("fetch", remote, base_branch)
        except utils.CommandError as e:
            console.print(f"[red]Failed to fetch from {remote}: {e}[/]")
            raise

    # Create the branch from the fetched base
    base_ref = f"{remote}/{base_branch}"
    with console.status(f"Creating branch '{name}' from {base_ref}..."):
        try:
            if checkout:
                await utils.git("checkout", "--track", "-b", name, base_ref)
            else:
                await utils.git("branch", "--track", name, base_ref)
        except utils.CommandError as e:
            console.print(f"[red]Failed to create branch '{name}': {e}[/]")
            sys.exit(1)

    console.print(f"[green]Created branch '{name}' tracking {base_ref}[/]")

    if checkout:
        console.print(f"[green]Switched to branch '{name}'[/]")
    else:
        console.print(f"[dim]Run 'git checkout {name}' to switch to the new branch[/]")
