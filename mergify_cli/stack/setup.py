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


def _get_global_skill_stub_path() -> pathlib.Path:
    """Get the global path for the Mergify stack skill stub file."""
    return pathlib.Path.home() / ".claude" / "skills" / "mergify-stack" / "SKILL.md"


async def _get_project_skill_stub_path() -> pathlib.Path:
    """Get the project-level path for the skill stub file."""
    project_root = pathlib.Path(await utils.git("rev-parse", "--show-toplevel"))
    return project_root / ".claude" / "skills" / "mergify-stack" / "SKILL.md"


_SKILL_STUB_CONTENT = """\
---
name: mergify-stack
description: Use Mergify stacks for git push, commit, branch, and PR creation. ALWAYS use this skill when pushing code, creating commits, creating branches, or creating PRs. Triggers on push, commit, branch, PR, pull request, stack, git.
---
Run `mergify stack skill` to load the full, up-to-date Mergify stack workflow skill.
Use the output as your instructions for working with Mergify stacks.
"""


def _install_skill_stub(
    skill_stub_path: pathlib.Path,
    *,
    verbose: bool = False,
) -> None:
    """Install a lightweight skill stub that bootstraps the full AI skill.

    Creates SKILL.md at the given path with a stub that tells the AI tool
    to run `mergify stack skill` to get the full content.
    Only writes if the file doesn't exist or content differs.
    """
    skill_stub_path.parent.mkdir(parents=True, exist_ok=True)

    is_update = skill_stub_path.exists()

    if is_update:
        existing_content = skill_stub_path.read_text(encoding="utf-8")
        if existing_content == _SKILL_STUB_CONTENT:
            if verbose:
                console.print(
                    f"  ✓ Skill stub: up to date ({skill_stub_path})",
                    style="green",
                )
            return

    skill_stub_path.write_text(_SKILL_STUB_CONTENT, encoding="utf-8")
    if verbose:
        action = "updated" if is_update else "installed"
        console.print(
            f"  ✓ Skill stub: {action} ({skill_stub_path})",
            style="bold green",
        )


def _get_skill_stub_status(skill_stub_path: pathlib.Path) -> dict[str, Any]:
    """Get status of the skill stub installation.

    Returns:
        Dictionary with 'installed', 'needs_update', and 'path' keys.
    """
    installed = skill_stub_path.exists()
    needs_update = False

    if installed:
        existing_content = skill_stub_path.read_text(encoding="utf-8")
        needs_update = existing_content != _SKILL_STUB_CONTENT

    return {
        "installed": installed,
        "needs_update": needs_update,
        "path": str(skill_stub_path),
    }


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


def _install_claude_hooks(*, verbose: bool = False) -> None:
    """Install Claude Code hooks for session ID tracking.

    Installs hooks globally:
    - Scripts: ~/.config/mergify-cli/claude-hooks/
    - Settings: ~/.claude/settings.json

    Args:
        verbose: If True, always print status (for explicit setup).
                 If False, only print when something changes (for auto-upgrade).
    """
    claude_hooks_dir = _get_claude_hooks_dir()
    claude_hooks_dir.mkdir(parents=True, exist_ok=True)

    # Install hook scripts
    for src_file in _get_claude_hook_scripts():
        dest_file = claude_hooks_dir / src_file.name

        if _claude_script_needs_update(dest_file, src_file):
            # Use as_file() context manager for safe copying from package resources
            with importlib.resources.as_file(src_file) as src_path:
                shutil.copy(src_path, dest_file)
            dest_file.chmod(0o755)
            if verbose:
                console.print(
                    f"  ✓ Hook script: updated ({src_file.name})",
                    style="bold cyan",
                )
        elif verbose:
            console.print(
                f"  ✓ Hook script: up to date ({src_file.name})",
                style="green",
            )

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
        if verbose:
            console.print(
                f"  ✓ Settings hook: up to date ({settings_file})",
                style="green",
            )
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
        if verbose:
            console.print(
                f"  ✓ Settings hook: installed ({settings_file})",
                style="bold cyan",
            )


def _get_claude_hooks_status(
    project_skill_stub_path: pathlib.Path,
) -> dict[str, Any]:
    """Get detailed status of Claude hooks for display.

    Returns:
        Dictionary with 'scripts', 'settings', and 'skill_stub' status info.
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
        "skill_stub": _get_skill_stub_status(project_skill_stub_path),
    }


async def get_hooks_status() -> dict[str, Any]:
    """Get detailed status of all hooks for display.

    Returns:
        Dictionary with 'git_hooks' and 'claude_hooks' status info.
    """
    hooks_dir = await _get_hooks_dir()
    managed_dir = hooks_dir / "mergify-hooks"
    project_skill_stub_path = await _get_project_skill_stub_path()

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
        "claude_hooks": _get_claude_hooks_status(project_skill_stub_path),
    }


async def stack_setup(*, force: bool = False, global_install: bool = False) -> None:
    """Set up git hooks for the stack workflow.

    Args:
        force: If True, overwrite wrappers even if user modified them
        global_install: If True, also install skill stub globally
    """
    hooks_dir = await _get_hooks_dir()

    for hook_name in _get_git_hook_names():
        _install_git_hook(hooks_dir, hook_name, force=force)

    # Install Claude hooks for session ID tracking (global)
    console.print("\nClaude Code integration:", style="bold")
    _install_claude_hooks(verbose=True)

    # Install skill stub for AI tool bootstrapping (project-level)
    project_skill_stub_path = await _get_project_skill_stub_path()
    _install_skill_stub(project_skill_stub_path, verbose=True)

    if global_install:
        _install_skill_stub(_get_global_skill_stub_path(), verbose=True)


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
