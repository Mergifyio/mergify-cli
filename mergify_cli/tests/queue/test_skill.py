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

import pathlib
import re
import shutil
import subprocess

import yaml


SKILL_PATH = (
    pathlib.Path(__file__).parents[3] / "skills" / "mergify-merge-queue" / "SKILL.md"
)


def _get_skill_content() -> str:
    return SKILL_PATH.read_text(encoding="utf-8")


def _native_commands_for_group(group: str) -> set[str]:
    """Ask the installed `mergify` binary which `<group> <sub>` pairs
    it handles natively, then return the subcommands for `group`.

    Spawning the binary keeps this test honest: the source of truth
    is the binary itself, so a port that adds a native subcommand
    (and its `NATIVE_COMMANDS` entry) automatically becomes visible
    here. No parallel hard-coded list to drift.
    """
    # Fail (don't skip) when `mergify` isn't on PATH: the wheel must
    # be installed for the test to be meaningful, and silently
    # skipping would let a regression in the wheel's bin scripts
    # slip through unnoticed. `uv run pytest` installs the wheel
    # before invoking pytest, so on every supported entry point the
    # binary is present.
    binary = shutil.which("mergify")
    assert binary is not None, (
        "`mergify` binary not on PATH. Install the wheel first "
        "(`uv run pytest` does this automatically) — running this "
        "test against an uninstalled checkout would give a false pass."
    )
    out = subprocess.run(
        [binary, "--list-native-commands"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    pairs = (line.split(maxsplit=1) for line in out.splitlines() if line.strip())
    return {
        sub
        for pair in pairs
        if len(pair) == 2 and pair[0] == group
        for sub in [pair[1]]
    }


def test_skill_content_is_readable() -> None:
    content = _get_skill_content()
    assert len(content) > 0


def test_skill_has_valid_frontmatter() -> None:
    content = _get_skill_content()
    # Extract YAML frontmatter between --- markers
    match = re.match(r"^---\n(.+?)\n---\n", content, re.DOTALL)
    assert match is not None, "Skill must have YAML frontmatter"

    frontmatter = yaml.safe_load(match.group(1))
    assert isinstance(frontmatter, dict), "Frontmatter must be a YAML mapping"
    assert "name" in frontmatter, "Frontmatter must have 'name'"
    assert "description" in frontmatter, "Frontmatter must have 'description'"
    assert frontmatter["name"] == "mergify-merge-queue"


REQUIRED_SECTIONS = [
    "## Commands",
    "## Checking Queue Status",
    "## Inspecting a PR",
    "## Queue States",
    "## Troubleshooting",
]


def test_skill_has_required_sections() -> None:
    content = _get_skill_content()
    for section in REQUIRED_SECTIONS:
        assert section in content, f"Skill is missing required section: {section}"


def test_skill_references_valid_commands() -> None:
    """Every `mergify queue <cmd>` reference in the skill must resolve
    to a Rust-native command reported by the binary. The whole
    `queue` group has been ported, so the binary is the only source
    of truth — no parallel click-command list to consult.
    """
    content = _get_skill_content()
    referenced = set(re.findall(r"mergify queue ([\w-]+)", content))
    available = _native_commands_for_group("queue")

    for cmd in referenced:
        assert cmd in available, (
            f"Skill references 'mergify queue {cmd}' but it's not a "
            f"Rust-native command reported by `mergify --list-native-commands`. "
            f"Available: {sorted(available)}"
        )
