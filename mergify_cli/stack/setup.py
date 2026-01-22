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

import enum
import importlib.resources
import json
import pathlib
import shutil

from mergify_cli import console
from mergify_cli import utils


class WrapperStatus(enum.Enum):
    """Status of an installed hook wrapper."""

    MISSING = "missing"  # Not installed at all
    LEGACY = "legacy"  # Old-style hook (pre-sourcing architecture)
    INSTALLED = "installed"  # New-style wrapper that sources from mergify-hooks/


def _get_git_hook_names() -> list[str]:
    """Get list of git hook names from the scripts directory."""
    scripts_dir = importlib.resources.files(__package__).joinpath("hooks/scripts")
    return [
        f.name.removesuffix(".sh")
        for f in scripts_dir.iterdir()
        if f.name.endswith(".sh")
    ]


def _get_claude_hooks_dir() -> pathlib.Path:
    """Get the global directory for Claude hook scripts."""
    return pathlib.Path.home() / ".config" / "mergify-cli" / "claude-hooks"


def _get_claude_settings_file() -> pathlib.Path:
    """Get the global Claude settings file path."""
    return pathlib.Path.home() / ".claude" / "settings.json"


def _get_wrapper_status(hook_path: pathlib.Path, hook_name: str) -> WrapperStatus:
    """Check the status of a hook wrapper."""
    if not hook_path.exists():
        return WrapperStatus.MISSING

    try:
        content = hook_path.read_text(encoding="utf-8")
    except OSError:
        return WrapperStatus.MISSING

    # Check if it's our new wrapper (sources from mergify-hooks/)
    if "mergify-hooks" in content and f"{hook_name}.sh" in content:
        return WrapperStatus.INSTALLED

    # Check if it's a legacy mergify hook
    if hook_name == "commit-msg" and "Change-Id: I${random}" in content:
        return WrapperStatus.LEGACY
    if hook_name == "prepare-commit-msg" and "is_amend_with_m_flag" in content:
        return WrapperStatus.LEGACY

    # User's own hook - treat as installed (don't touch it)
    return WrapperStatus.INSTALLED


def _script_needs_update(script_path: pathlib.Path, new_script_path: str) -> bool:
    """Check if a managed script needs to be updated by comparing content."""
    if not script_path.exists():
        return True

    try:
        installed_content = script_path.read_text(encoding="utf-8")
        new_content = pathlib.Path(new_script_path).read_text(encoding="utf-8")
    except OSError:
        return True
    else:
        return installed_content != new_content


def _install_git_hook(
    hooks_dir: pathlib.Path,
    hook_name: str,
    *,
    force: bool = False,
    check_only: bool = False,
) -> None:
    """Install or upgrade a git hook with the sourcing architecture.

    Structure:
    - .git/hooks/{hook_name} - Thin wrapper (installed once, user can modify)
    - .git/hooks/mergify-hooks/{hook_name}.sh - Managed script (always upgradable)
    """
    wrapper_path = hooks_dir / hook_name
    wrapper_status = _get_wrapper_status(wrapper_path, hook_name)

    managed_dir = hooks_dir / "mergify-hooks"
    script_path = managed_dir / f"{hook_name}.sh"
    new_script_file = str(
        importlib.resources.files(__package__).joinpath(
            f"hooks/scripts/{hook_name}.sh",
        ),
    )
    new_wrapper_file = str(
        importlib.resources.files(__package__).joinpath(
            f"hooks/wrappers/{hook_name}",
        ),
    )

    if check_only:
        # Just report status
        if wrapper_status == WrapperStatus.MISSING:
            console.log(f"Hook not installed: {hook_name}")
        elif wrapper_status == WrapperStatus.LEGACY:
            console.log(f"Legacy hook found: {hook_name} (use --force to migrate)")
        elif _script_needs_update(script_path, new_script_file):
            console.log(f"Hook script needs update: {hook_name}")
        else:
            console.log(f"Hook is up to date: {hook_name}")
        return

    # Create mergify-hooks directory
    managed_dir.mkdir(exist_ok=True)

    # Always update managed script if content differs
    if _script_needs_update(script_path, new_script_file):
        console.log(f"Updating managed hook script: mergify-hooks/{hook_name}.sh")
        shutil.copy(new_script_file, script_path)
        script_path.chmod(0o755)
    elif utils.is_debug():
        console.log(f"Managed hook script is up to date: mergify-hooks/{hook_name}.sh")

    # Handle wrapper based on status
    if wrapper_status == WrapperStatus.MISSING:
        console.log(f"Installing hook wrapper: {hook_name}")
        shutil.copy(new_wrapper_file, wrapper_path)
        wrapper_path.chmod(0o755)

    elif wrapper_status == WrapperStatus.LEGACY:
        if force:
            console.log(f"Migrating legacy hook to new format: {hook_name}")
            shutil.copy(new_wrapper_file, wrapper_path)
            wrapper_path.chmod(0o755)
        else:
            console.print(
                f"Found legacy hook: {hook_name}\n"
                f"Run 'mergify stack setup --force' to migrate to new format.",
                style="yellow",
            )

    elif utils.is_debug():
        console.log(f"Hook wrapper already installed: {hook_name}")


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

        # Check if update needed by comparing content directly
        # Use read_text() on Traversable to handle zip-packaged installations
        needs_update = True
        if dest_file.exists():
            installed_content = dest_file.read_text(encoding="utf-8")
            new_content = src_file.read_text(encoding="utf-8")
            needs_update = installed_content != new_content

        if needs_update:
            console.log(f"Updating Claude hook script: {src_file.name}")
            # Use as_file() context manager for safe copying from package resources
            with importlib.resources.as_file(src_file) as src_path:
                shutil.copy(src_path, dest_file)
            dest_file.chmod(0o755)
        elif utils.is_debug():
            console.log(f"Claude hook script is up to date: {src_file.name}")

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

    def _get_hook_command(hook: dict[str, object]) -> str:
        """Safely extract command from hook structure, handling empty lists."""
        hooks_list = hook.get("hooks", [])
        if not hooks_list or not isinstance(hooks_list, list):
            return ""
        first_hook = hooks_list[0]
        if not isinstance(first_hook, dict):
            return ""
        command = first_hook.get("command", "")
        return command if isinstance(command, str) else ""

    # Check if our hook is already installed (by checking the command)
    already_installed = any(
        _get_hook_command(hook) == hook_script_path for hook in existing_hooks
    )

    if already_installed:
        if utils.is_debug():
            console.log("Claude settings.json hook is up to date")
    else:
        # Remove any old mergify-cli hooks that might reference different paths
        # Be specific: only remove hooks that contain "mergify-cli" in the path
        filtered_hooks = [
            hook
            for hook in existing_hooks
            if not (
                "mergify-cli" in _get_hook_command(hook)
                and _get_hook_command(hook).endswith("session-start.sh")
            )
        ]
        existing_settings["hooks"]["SessionStart"] = filtered_hooks + our_hook
        settings_file.write_text(
            json.dumps(existing_settings, indent=2) + "\n",
            encoding="utf-8",
        )
        console.log("Installation of Claude settings.json hook")


async def stack_setup(*, force: bool = False, check_only: bool = False) -> None:
    """Set up git hooks for the stack workflow.

    Args:
        force: If True, overwrite wrappers even if user modified them
        check_only: If True, only check status without making changes
    """
    hooks_dir = pathlib.Path(await utils.git("rev-parse", "--git-path", "hooks"))

    for hook_name in _get_git_hook_names():
        _install_git_hook(hooks_dir, hook_name, force=force, check_only=check_only)

    if not check_only:
        # Install Claude hooks for session ID tracking (global)
        _install_claude_hooks()


async def ensure_hooks_updated() -> None:
    """Ensure hooks are up to date, called automatically by stack commands.

    This only updates the managed scripts, never touches user's wrapper files.
    """
    hooks_dir = pathlib.Path(await utils.git("rev-parse", "--git-path", "hooks"))
    managed_dir = hooks_dir / "mergify-hooks"

    # Update git hook scripts
    if managed_dir.exists():
        for hook_name in _get_git_hook_names():
            script_path = managed_dir / f"{hook_name}.sh"
            new_script_file = str(
                importlib.resources.files(__package__).joinpath(
                    f"hooks/scripts/{hook_name}.sh",
                ),
            )

            if _script_needs_update(script_path, new_script_file):
                console.log(
                    f"Auto-updating managed hook script: mergify-hooks/{hook_name}.sh",
                )
                shutil.copy(new_script_file, script_path)
                script_path.chmod(0o755)

    # Install/update Claude hook scripts (always, creates directory if needed)
    _install_claude_hooks()
