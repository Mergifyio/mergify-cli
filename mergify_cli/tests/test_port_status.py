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
"""Port inventory guard.

Walks the click command tree exposed by ``mergify_cli.cli.cli`` and
compares it to the inventory in ``PORT_STATUS.toml``. Any mismatch
is a CI failure.

The intent is to prevent a Python command from being added while
the Rust port is in flight without someone explicitly deciding
whether it ships via the shim (``status = "shimmed"``) or via a
native Rust implementation (``status = "native"``). Forgetting to
port a new command therefore surfaces immediately rather than
getting noticed months later when users report missing
functionality in the static binary.
"""

from __future__ import annotations

import pathlib
import tomllib

import click

from mergify_cli.cli import cli as _cli


_VALID_STATUSES: frozenset[str] = frozenset({"native", "shimmed"})
_PORT_STATUS_PATH = (
    pathlib.Path(__file__).resolve().parent.parent.parent / "PORT_STATUS.toml"
)


def _walk_commands(
    cmd: click.Command,
    prefix: tuple[str, ...] = (),
) -> list[tuple[str, ...]]:
    """Collect the path of every leaf command reachable from ``cmd``.

    Groups contribute nothing themselves — only their leaf
    subcommands appear. Empty prefixes mean "the root `mergify`
    command invoked without a subcommand", which we don't track.
    """
    if isinstance(cmd, click.Group):
        paths: list[tuple[str, ...]] = []
        for name, child in sorted(cmd.commands.items()):
            paths.extend(_walk_commands(child, (*prefix, name)))
        return paths
    return [prefix] if prefix else []


def _discovered_commands() -> set[tuple[str, ...]]:
    return set(_walk_commands(_cli))


def _load_port_status() -> list[dict[str, object]]:
    text = _PORT_STATUS_PATH.read_text(encoding="utf-8")
    data = tomllib.loads(text)
    commands = data.get("command", [])
    assert isinstance(commands, list), (
        "PORT_STATUS.toml must define `command` as an array of tables "
        "using `[[command]]`, not a single table `[command]`."
    )
    assert all(isinstance(entry, dict) for entry in commands), (
        "PORT_STATUS.toml `command` entries must each be tables defined "
        "with `[[command]]`."
    )
    return commands


def _declared_commands() -> set[tuple[str, ...]]:
    return {tuple(entry["path"]) for entry in _load_port_status()}  # type: ignore[arg-type]


def test_every_python_command_is_in_port_status() -> None:
    """Every click command exposed by the Python CLI must appear in
    PORT_STATUS.toml."""
    discovered = _discovered_commands()
    declared = _declared_commands()

    missing = discovered - declared
    assert not missing, (
        "\nThese click commands exist in mergify_cli but are not listed "
        "in PORT_STATUS.toml:\n"
        + "\n".join(f"  - {' '.join(path)}" for path in sorted(missing))
        + '\n\nAdd each as `status = "shimmed"` (or `status = "native"` '
        "if already ported) so the Rust port doesn't forget them."
    )


def test_no_stale_entries_in_port_status() -> None:
    """Every entry in PORT_STATUS.toml must correspond to a live
    click command."""
    discovered = _discovered_commands()
    declared = _declared_commands()

    extra = declared - discovered
    assert not extra, (
        "\nThese entries in PORT_STATUS.toml do not match any "
        "click command:\n"
        + "\n".join(f"  - {' '.join(path)}" for path in sorted(extra))
        + "\n\nRemove the stale entries (the command was renamed or "
        "deleted)."
    )


def test_port_status_uses_only_valid_status_values() -> None:
    """Every entry must use a known status value."""
    for entry in _load_port_status():
        # Validate required keys here so a typo in `path` or `status`
        # surfaces with a targeted assertion message instead of a
        # bare KeyError traceback.
        assert "path" in entry, (
            f"PORT_STATUS.toml entry {entry!r} is missing required key 'path'"
        )
        assert "status" in entry, (
            f"PORT_STATUS.toml entry {entry!r} is missing required key 'status'"
        )
        path = entry["path"]
        assert isinstance(path, list), (
            f"PORT_STATUS.toml entry {entry!r}: 'path' must be a list"
        )
        assert all(isinstance(p, str) for p in path), (
            f"PORT_STATUS.toml entry {entry!r}: every 'path' segment must be a string"
        )
        status = entry["status"]
        assert status in _VALID_STATUSES, (
            f"PORT_STATUS.toml entry for {path!r} uses invalid "
            f"status {status!r}; valid values are "
            f"{sorted(_VALID_STATUSES)}"
        )


def test_port_status_entries_have_exactly_path_and_status_keys() -> None:
    """Catches typos like `stats` or accidentally adding a third
    undocumented key."""
    allowed = {"path", "status"}
    for entry in _load_port_status():
        actual = set(entry.keys())
        missing = allowed - actual
        extras = actual - allowed
        assert actual == allowed, (
            f"PORT_STATUS.toml entry {entry!r} must have exactly keys "
            f"{sorted(allowed)}; missing keys: {sorted(missing)}, "
            f"unexpected keys: {sorted(extras)}."
        )
