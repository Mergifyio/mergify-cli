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

from __future__ import annotations

import importlib.metadata
import json
import pathlib
import shutil
import sys

import aiofiles

from mergify_cli import console
from mergify_cli import utils


async def _install_hook(hooks_dir: pathlib.Path, hook_name: str) -> None:
    installed_hook_file = hooks_dir / hook_name

    new_hook_file = str(
        importlib.resources.files(__package__).joinpath(f"hooks/{hook_name}"),
    )

    if installed_hook_file.exists():
        async with aiofiles.open(installed_hook_file) as f:
            data_installed = await f.read()
        async with aiofiles.open(new_hook_file) as f:
            data_new = await f.read()
        if data_installed == data_new:
            console.log(f"Git {hook_name} hook is up to date")
        else:
            console.print(
                f"error: {installed_hook_file} differ from mergify_cli hook",
                style="red",
            )
            sys.exit(1)

    else:
        console.log(f"Installation of git {hook_name} hook")
        shutil.copy(new_hook_file, installed_hook_file)
        installed_hook_file.chmod(0o755)


async def _install_claude_hooks(project_dir: pathlib.Path) -> None:
    """Install Claude Code hooks for session ID tracking.

    Uses settings.local.json (gitignored) rather than settings.json
    so each user must run setup, similar to git hooks.
    """
    claude_dir = project_dir / ".claude"
    claude_hooks_dir = claude_dir / "hooks"

    # Create directories if they don't exist
    claude_hooks_dir.mkdir(parents=True, exist_ok=True)

    # Ensure hooks directory is gitignored
    gitignore_file = claude_dir / ".gitignore"
    hooks_pattern = "hooks/"
    if gitignore_file.exists():
        async with aiofiles.open(gitignore_file) as f:
            gitignore_content = await f.read()
        if hooks_pattern not in gitignore_content.splitlines():
            async with aiofiles.open(gitignore_file, "a") as f:
                await f.write(f"{hooks_pattern}\n")
            console.log("Added hooks/ to .claude/.gitignore")
    else:
        async with aiofiles.open(gitignore_file, "w") as f:
            await f.write(f"{hooks_pattern}\n")
        console.log("Created .claude/.gitignore with hooks/")

    # Load our hook configuration
    new_settings_file = str(
        importlib.resources.files(__package__).joinpath("claude_hooks/settings.json"),
    )
    async with aiofiles.open(new_settings_file) as f:
        new_settings = json.loads(await f.read())

    # Merge into settings.local.json (user-local, gitignored)
    settings_file = claude_dir / "settings.local.json"
    if settings_file.exists():
        async with aiofiles.open(settings_file) as f:
            try:
                existing_settings = json.loads(await f.read())
            except json.JSONDecodeError:
                existing_settings = {}
    else:
        existing_settings = {}

    # Merge hooks - add our SessionStart hook if not already present
    if "hooks" not in existing_settings:
        existing_settings["hooks"] = {}

    our_hook = new_settings["hooks"]["SessionStart"]
    existing_hooks = existing_settings["hooks"].get("SessionStart", [])

    # Check if our hook is already installed (by checking the command)
    our_command = our_hook[0]["hooks"][0]["command"]
    already_installed = any(
        hook.get("hooks", [{}])[0].get("command") == our_command
        for hook in existing_hooks
        if hook.get("hooks")
    )

    if already_installed:
        console.log("Claude settings.local.json hook is up to date")
    else:
        existing_settings["hooks"]["SessionStart"] = existing_hooks + our_hook
        async with aiofiles.open(settings_file, "w") as f:
            await f.write(json.dumps(existing_settings, indent=2) + "\n")
        console.log("Installation of Claude settings.local.json hook")

    # Install session-start.sh hook script
    hook_file = claude_hooks_dir / "session-start.sh"
    new_hook_file = str(
        importlib.resources.files(__package__).joinpath(
            "claude_hooks/session-start.sh",
        ),
    )

    if hook_file.exists():
        async with aiofiles.open(hook_file) as f:
            data_installed = await f.read()
        async with aiofiles.open(new_hook_file) as f:
            data_new = await f.read()
        if data_installed == data_new:
            console.log("Claude session-start.sh hook is up to date")
        else:
            console.print(
                f"warning: {hook_file} differs from mergify_cli hook, skipping",
                style="yellow",
            )
    else:
        console.log("Installation of Claude session-start.sh hook")
        shutil.copy(new_hook_file, hook_file)
        hook_file.chmod(0o755)


async def stack_setup() -> None:
    # Install git hooks
    hooks_dir = pathlib.Path(await utils.git("rev-parse", "--git-path", "hooks"))
    await _install_hook(hooks_dir, "commit-msg")
    await _install_hook(hooks_dir, "prepare-commit-msg")

    # Install Claude hooks for session ID tracking
    project_dir = pathlib.Path(await utils.git("rev-parse", "--show-toplevel"))
    await _install_claude_hooks(project_dir)
