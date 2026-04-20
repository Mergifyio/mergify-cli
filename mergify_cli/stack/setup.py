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
import pathlib
import shutil
from typing import Any

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


async def get_hooks_status() -> dict[str, Any]:
    """Get detailed status of all hooks for display.

    Returns:
        Dictionary with 'git_hooks' status info.
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
    }


async def _ensure_notes_display_ref() -> None:
    """Configure ``notes.displayRef`` so ``git log`` shows mergify notes."""
    desired = "refs/notes/mergify/*"
    try:
        current = await utils.git(
            "config",
            "--local",
            "--get-all",
            "notes.displayRef",
        )
        current_refs = current.splitlines()
    except utils.CommandError:
        current_refs = []
    if desired not in current_refs:
        await utils.git("config", "--local", "--add", "notes.displayRef", desired)
        console.log(f"Added notes.displayRef = {desired}")


async def stack_setup(*, force: bool = False) -> None:
    """Set up git hooks for the stack workflow.

    Args:
        force: If True, overwrite wrappers even if user modified them
    """
    hooks_dir = await _get_hooks_dir()

    for hook_name in _get_git_hook_names():
        _install_git_hook(hooks_dir, hook_name, force=force)

    await _ensure_notes_display_ref()


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
