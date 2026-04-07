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
from typing import TYPE_CHECKING

import yaml

from mergify_cli.stack import setup as stack_setup_mod
from mergify_cli.stack import skill as stack_skill_mod


if TYPE_CHECKING:
    import pathlib


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


def test_skill_stub_has_valid_frontmatter() -> None:
    """Verify the skill stub has valid YAML frontmatter and references mergify stack skill."""
    content = stack_setup_mod._SKILL_STUB_CONTENT
    # Extract YAML frontmatter between --- markers
    match = re.match(r"^---\n(.+?)\n---\n", content, re.DOTALL)
    assert match is not None, "Skill stub must have YAML frontmatter"

    frontmatter = yaml.safe_load(match.group(1))
    assert isinstance(frontmatter, dict), "Frontmatter must be a YAML mapping"
    assert "name" in frontmatter, "Frontmatter must have 'name'"
    assert "description" in frontmatter, "Frontmatter must have 'description'"
    assert frontmatter["name"] == "mergify-stack"

    # Verify the stub references the command to load full skill
    assert "mergify stack skill" in content, (
        "Skill stub must reference 'mergify stack skill' command"
    )


def test_skill_stub_install(
    tmp_path: pathlib.Path,
) -> None:
    """Verify _install_skill_stub creates the file with correct content."""
    stub_path = tmp_path / ".claude" / "skills" / "mergify-stack" / "SKILL.md"
    assert not stub_path.exists()

    stack_setup_mod._install_skill_stub(stub_path)

    assert stub_path.exists()
    content = stub_path.read_text(encoding="utf-8")
    assert content == stack_setup_mod._SKILL_STUB_CONTENT

    # Calling again should be a no-op (idempotent)
    stack_setup_mod._install_skill_stub(stub_path)
    # Content is unchanged, so file should remain the same
    assert stub_path.read_text(encoding="utf-8") == content


def test_skill_stub_updates_when_content_differs(
    tmp_path: pathlib.Path,
) -> None:
    """Verify _install_skill_stub updates the file when content differs."""
    stub_path = tmp_path / ".claude" / "skills" / "mergify-stack" / "SKILL.md"
    stub_path.parent.mkdir(parents=True)
    stub_path.write_text("old content", encoding="utf-8")

    stack_setup_mod._install_skill_stub(stub_path)

    assert stub_path.read_text(encoding="utf-8") == stack_setup_mod._SKILL_STUB_CONTENT
