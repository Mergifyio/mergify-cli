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

import enum
import importlib.resources
import importlib.resources.abc
import json
import pathlib
import shutil
from typing import TYPE_CHECKING
from typing import Any


if TYPE_CHECKING:
    from collections.abc import Iterator

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


async def _get_hooks_dir() -> pathlib.Path:
    """Get the git hooks directory for the current repository."""
    return pathlib.Path(await utils.git("rev-parse", "--git-path", "hooks"))


def _get_script_resource(hook_name: str) -> importlib.resources.abc.Traversable:
    """Get the Traversable resource for a hook script."""
    return importlib.resources.files(__package__).joinpath(
        f"hooks/scripts/{hook_name}.sh",
    )


def _get_wrapper_resource(hook_name: str) -> importlib.resources.abc.Traversable:
    """Get the Traversable resource for a hook wrapper."""
    return importlib.resources.files(__package__).joinpath(
        f"hooks/wrappers/{hook_name}",
    )


def _get_claude_hooks_dir() -> pathlib.Path:
    """Get the global directory for Claude hook scripts."""
    return pathlib.Path.home() / ".config" / "mergify-cli" / "claude-hooks"


def _get_claude_hook_scripts() -> Iterator[importlib.resources.abc.Traversable]:
    """Iterate over Claude hook script files in package resources."""
    claude_hooks_src = importlib.resources.files(__package__).joinpath("claude_hooks")
    for src_file in claude_hooks_src.iterdir():
        if src_file.name.endswith(".sh"):
            yield src_file


def _claude_script_needs_update(
    dest_file: pathlib.Path,
    src_file: importlib.resources.abc.Traversable,
) -> bool:
    """Check if a Claude hook script needs to be updated by comparing content."""
    if not dest_file.exists():
        return True
    installed_content = dest_file.read_text(encoding="utf-8")
    new_content = src_file.read_text(encoding="utf-8")
    return installed_content != new_content


def _get_hook_command(hook: dict[str, object]) -> str:
    """Safely extract command from Claude hook structure, handling empty lists."""
    hooks_list = hook.get("hooks", [])
    if not hooks_list or not isinstance(hooks_list, list):
        return ""
    first_hook = hooks_list[0]
    if not isinstance(first_hook, dict):
        return ""
    command = first_hook.get("command", "")
    return command if isinstance(command, str) else ""


def _get_claude_settings_file() -> pathlib.Path:
    """Get the global Claude settings file path."""
    return pathlib.Path.home() / ".claude" / "settings.json"


def _read_claude_settings() -> dict[str, Any]:
    """Read and parse Claude settings.json, returning empty dict on failure."""
    settings_file = _get_claude_settings_file()
    if not settings_file.exists():
        return {}
    try:
        result: dict[str, Any] = json.loads(settings_file.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, OSError):
        return {}
    else:
        return result


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


def _script_needs_update(
    script_path: pathlib.Path,
    new_script: importlib.resources.abc.Traversable,
) -> bool:
    """Check if a managed script needs to be updated by comparing content."""
    if not script_path.exists():
        return True

    try:
        installed_content = script_path.read_text(encoding="utf-8")
        new_content = new_script.read_text(encoding="utf-8")
    except OSError:
        return True
    else:
        return installed_content != new_content


def _install_git_hook(
    hooks_dir: pathlib.Path,
    hook_name: str,
    *,
    force: bool = False,
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
    new_script_resource = _get_script_resource(hook_name)
    new_wrapper_resource = _get_wrapper_resource(hook_name)

    # Create mergify-hooks directory
    managed_dir.mkdir(exist_ok=True)

    # Always update managed script if content differs
    if _script_needs_update(script_path, new_script_resource):
        console.log(f"Updating managed hook script: mergify-hooks/{hook_name}.sh")
        with importlib.resources.as_file(new_script_resource) as src_path:
            shutil.copy(src_path, script_path)
        script_path.chmod(0o755)
    elif utils.is_debug():
        console.log(f"Managed hook script is up to date: mergify-hooks/{hook_name}.sh")

    # Handle wrapper based on status
    if wrapper_status == WrapperStatus.MISSING:
        console.log(f"Installing hook wrapper: {hook_name}")
        with importlib.resources.as_file(new_wrapper_resource) as src_path:
            shutil.copy(src_path, wrapper_path)
        wrapper_path.chmod(0o755)

    elif wrapper_status == WrapperStatus.LEGACY:
        if force:
            console.log(f"Migrating legacy hook to new format: {hook_name}")
            with importlib.resources.as_file(new_wrapper_resource) as src_path:
                shutil.copy(src_path, wrapper_path)
            wrapper_path.chmod(0o755)
        else:
            console.print(
                f"Found legacy hook: {hook_name}\n"
                f"Run 'mergify stack hooks --setup --force' to migrate to new format.",
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
    for src_file in _get_claude_hook_scripts():
        dest_file = claude_hooks_dir / src_file.name

        if _claude_script_needs_update(dest_file, src_file):
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
    existing_settings = _read_claude_settings()

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


def _get_claude_hooks_status() -> dict[str, Any]:
    """Get detailed status of Claude hooks for display.

    Returns:
        Dictionary with 'scripts' and 'settings' status info.
    """
    claude_hooks_dir = _get_claude_hooks_dir()
    settings_file = _get_claude_settings_file()

    # Check script status
    scripts_status = {}
    for src_file in _get_claude_hook_scripts():
        dest_file = claude_hooks_dir / src_file.name
        installed = dest_file.exists()
        needs_update = (
            _claude_script_needs_update(dest_file, src_file) if installed else False
        )

        scripts_status[src_file.name] = {
            "installed": installed,
            "needs_update": needs_update,
            "path": str(dest_file),
        }

    # Check settings.json status
    hook_script_path = str(claude_hooks_dir / "session-start.sh")
    existing_settings = _read_claude_settings()
    existing_hooks = existing_settings.get("hooks", {}).get("SessionStart", [])
    settings_installed = any(
        _get_hook_command(hook) == hook_script_path for hook in existing_hooks
    )

    return {
        "scripts": scripts_status,
        "settings_installed": settings_installed,
        "settings_path": str(settings_file),
    }


async def get_hooks_status() -> dict[str, Any]:
    """Get detailed status of all hooks for display.

    Returns:
        Dictionary with 'git_hooks' and 'claude_hooks' status info.
    """
    hooks_dir = await _get_hooks_dir()
    managed_dir = hooks_dir / "mergify-hooks"

    git_hooks = {}
    for hook_name in _get_git_hook_names():
        wrapper_path = hooks_dir / hook_name
        script_path = managed_dir / f"{hook_name}.sh"

        wrapper_status = _get_wrapper_status(wrapper_path, hook_name)
        script_installed = script_path.exists()
        script_needs_update = False

        if script_installed:
            new_script_resource = _get_script_resource(hook_name)
            script_needs_update = _script_needs_update(script_path, new_script_resource)

        git_hooks[hook_name] = {
            "wrapper_status": wrapper_status,
            "script_installed": script_installed,
            "script_needs_update": script_needs_update,
            "wrapper_path": str(wrapper_path),
            "script_path": str(script_path),
        }

    return {
        "git_hooks": git_hooks,
        "claude_hooks": _get_claude_hooks_status(),
    }


async def stack_setup(*, force: bool = False) -> None:
    """Set up git hooks for the stack workflow.

    Args:
        force: If True, overwrite wrappers even if user modified them
    """
    hooks_dir = await _get_hooks_dir()

    for hook_name in _get_git_hook_names():
        _install_git_hook(hooks_dir, hook_name, force=force)

    # Install Claude hooks for session ID tracking (global)
    _install_claude_hooks()


async def ensure_hooks_updated() -> None:
    """Ensure hooks are up to date, called automatically by stack commands.

    This only updates the managed scripts, never touches user's wrapper files.
    """
    hooks_dir = await _get_hooks_dir()
    managed_dir = hooks_dir / "mergify-hooks"

    # Update git hook scripts
    if managed_dir.exists():
        for hook_name in _get_git_hook_names():
            script_path = managed_dir / f"{hook_name}.sh"
            new_script_resource = _get_script_resource(hook_name)

            if _script_needs_update(script_path, new_script_resource):
                console.log(
                    f"Auto-updating managed hook script: mergify-hooks/{hook_name}.sh",
                )
                with importlib.resources.as_file(new_script_resource) as src_path:
                    shutil.copy(src_path, script_path)
                script_path.chmod(0o755)

    # Install/update Claude hook scripts (always, creates directory if needed)
    _install_claude_hooks()
