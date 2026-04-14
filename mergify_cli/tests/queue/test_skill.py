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

import yaml


SKILL_PATH = (
    pathlib.Path(__file__).parents[3] / "skills" / "mergify-merge-queue" / "SKILL.md"
)


def _get_skill_content() -> str:
    return SKILL_PATH.read_text(encoding="utf-8")


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
    """Check that commands referenced in the skill exist in the CLI."""
    from mergify_cli.queue.cli import queue

    content = _get_skill_content()
    # Extract `mergify queue <subcommand>` references
    referenced = set(re.findall(r"mergify queue ([\w-]+)", content))

    available = set(queue.commands.keys())

    for cmd in referenced:
        assert cmd in available, (
            f"Skill references 'mergify queue {cmd}' but it's not a registered command. "
            f"Available: {sorted(available)}"
        )
