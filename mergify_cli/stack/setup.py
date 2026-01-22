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

import importlib.resources
import json
import pathlib
import shutil
import sys

import aiofiles

from mergify_cli import console
from mergify_cli import utils


def _get_claude_hooks_dir() -> pathlib.Path:
    """Get the global directory for Claude hook scripts."""
    return pathlib.Path.home() / ".config" / "mergify-cli" / "claude-hooks"


def _get_claude_settings_file() -> pathlib.Path:
    """Get the global Claude settings file path."""
    return pathlib.Path.home() / ".claude" / "settings.json"


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


def _install_claude_hooks() -> None:
    """Install Claude Code hooks for session ID tracking.

    Installs hooks globally:
    - Scripts: ~/.config/mergify-cli/claude-hooks/
    - Settings: ~/.claude/settings.json
    """
    claude_hooks_dir = _get_claude_hooks_dir()
    claude_hooks_dir.mkdir(parents=True, exist_ok=True)

    # Install hook scripts
    claude_hooks_src = importlib.resources.files(__package__).joinpath("claude_hooks")
    for src_file in claude_hooks_src.iterdir():
        if not src_file.name.endswith(".sh"):
            continue

        dest_file = claude_hooks_dir / src_file.name
        src_path = str(src_file)

        if dest_file.exists():
            installed_content = dest_file.read_text(encoding="utf-8")
            new_content = pathlib.Path(src_path).read_text(encoding="utf-8")
            if installed_content == new_content:
                console.log(f"Claude hook script is up to date: {src_file.name}")
                continue

        console.log(f"Installing Claude hook script: {src_file.name}")
        shutil.copy(src_path, dest_file)
        dest_file.chmod(0o755)

    # Install/update Claude settings
    settings_file = _get_claude_settings_file()
    settings_file.parent.mkdir(parents=True, exist_ok=True)

    if settings_file.exists():
        try:
            existing_settings = json.loads(
                settings_file.read_text(encoding="utf-8"),
            )
        except json.JSONDecodeError:
            existing_settings = {}
    else:
        existing_settings = {}

    if "hooks" not in existing_settings:
        existing_settings["hooks"] = {}

    # Build our hook configuration with absolute path
    hook_script_path = str(claude_hooks_dir / "session-start.sh")
    our_hook = [
        {
            "hooks": [
                {
                    "type": "command",
                    "command": hook_script_path,
                },
            ],
        },
    ]

    existing_hooks = existing_settings["hooks"].get("SessionStart", [])

    # Check if our hook is already installed (by checking the command)
    already_installed = any(
        hook.get("hooks", [{}])[0].get("command") == hook_script_path
        for hook in existing_hooks
        if hook.get("hooks")
    )

    if already_installed:
        console.log("Claude settings.json hook is up to date")
    else:
        # Remove any old mergify-cli hooks that might reference different paths
        filtered_hooks = [
            hook
            for hook in existing_hooks
            if not (
                hook.get("hooks", [{}])[0]
                .get("command", "")
                .endswith("session-start.sh")
            )
        ]
        existing_settings["hooks"]["SessionStart"] = filtered_hooks + our_hook
        settings_file.write_text(
            json.dumps(existing_settings, indent=2) + "\n",
            encoding="utf-8",
        )
        console.log("Installation of Claude settings.json hook")


async def stack_setup() -> None:
    # Install git hooks
    hooks_dir = pathlib.Path(await utils.git("rev-parse", "--git-path", "hooks"))
    await _install_hook(hooks_dir, "commit-msg")
    await _install_hook(hooks_dir, "prepare-commit-msg")

    # Install Claude hooks for session ID tracking (global)
    _install_claude_hooks()
