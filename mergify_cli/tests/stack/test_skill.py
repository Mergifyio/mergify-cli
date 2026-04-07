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

import re

import yaml

from mergify_cli.stack import skill as stack_skill_mod


def test_skill_content_is_readable() -> None:
    content = stack_skill_mod.get_skill_content()
    assert len(content) > 0


def test_skill_has_valid_frontmatter() -> None:
    content = stack_skill_mod.get_skill_content()
    # Extract YAML frontmatter between --- markers
    match = re.match(r"^---\n(.+?)\n---\n", content, re.DOTALL)
    assert match is not None, "Skill must have YAML frontmatter"

    frontmatter = yaml.safe_load(match.group(1))
    assert isinstance(frontmatter, dict), "Frontmatter must be a YAML mapping"
    assert "name" in frontmatter, "Frontmatter must have 'name'"
    assert "description" in frontmatter, "Frontmatter must have 'description'"
    assert frontmatter["name"] == "mergify-stack"


REQUIRED_SECTIONS = [
    "## Core Conventions",
    "## Commands",
    "## Starting New Work",
]


def test_skill_has_required_sections() -> None:
    content = stack_skill_mod.get_skill_content()
    for section in REQUIRED_SECTIONS:
        assert section in content, f"Skill is missing required section: {section}"


def test_skill_references_valid_commands() -> None:
    """Check that commands referenced in the skill exist in the CLI."""
    from mergify_cli.stack.cli import stack

    content = stack_skill_mod.get_skill_content()
    # Extract `mergify stack <subcommand>` references
    referenced = set(re.findall(r"mergify stack ([\w-]+)", content))

    available = set(stack.commands.keys())

    for cmd in referenced:
        assert cmd in available, (
            f"Skill references 'mergify stack {cmd}' but it's not a registered command. "
            f"Available: {sorted(available)}"
        )
