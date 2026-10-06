#!/usr/bin/env python3
"""Check the agent skills under skills/ against a mergify binary.

Usage: check-skills.py <path-to-mergify-binary>

The skills tell an agent which `mergify` commands to run, so a skill that names
a command the binary doesn't have sends the agent into an error. The CLI's
source lives in a private repository now; this runs against a released binary
(CI uses the latest release's), the one people actually get.

For every skills/<name>/SKILL.md:
- YAML front matter between `---` lines, with `name: <name>` matching the
  directory and a `description`, which is what agent skill loaders read;
- every `mergify <group> <subcommand>` it mentions, for a group the binary
  has, is a subcommand the binary lists in `--list-native-commands`.
The merge-queue skill must also keep its required sections.
"""

import re
import subprocess
import sys
from pathlib import Path

SKILLS = Path(__file__).resolve().parent.parent / "skills"

REQUIRED_SECTIONS = {
    "mergify-merge-queue": [
        "## Commands",
        "## Checking Queue Status",
        "## Inspecting a PR",
        "## Queue States",
        "## Troubleshooting",
    ],
}

FRONT_MATTER = re.compile(r"\A---\n(.+?)\n---\n", re.DOTALL)
REFERENCE = re.compile(r"\bmergify ([a-z][\w-]*) ([a-z][\w-]*)")


def native_commands(binary: str) -> dict[str, set[str]]:
    output = subprocess.run(
        [binary, "--list-native-commands"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    commands: dict[str, set[str]] = {}
    for line in output.splitlines():
        group, _, sub = line.partition(" ")
        if sub:
            commands.setdefault(group, set()).add(sub.strip())
    return commands


def check_skill(path: Path, commands: dict[str, set[str]]) -> list[str]:
    name = path.parent.name
    content = path.read_text(encoding="utf-8")
    problems = []

    match = FRONT_MATTER.match(content)
    if not match:
        return [f"{name}: no YAML front matter between --- lines"]
    keys = {}
    for line in match.group(1).splitlines():
        key, sep, value = line.partition(":")
        if sep and not line.startswith((" ", "\t")):
            keys[key.strip()] = value.strip()
    if keys.get("name") != name:
        problems.append(f"{name}: front matter name is {keys.get('name')!r}, expected {name!r}")
    if not keys.get("description"):
        problems.append(f"{name}: front matter has no description")

    for section in REQUIRED_SECTIONS.get(name, []):
        if section not in content:
            problems.append(f"{name}: missing required section {section!r}")

    for group, sub in sorted(set(REFERENCE.findall(content))):
        if group in commands and sub not in commands[group]:
            problems.append(
                f"{name}: mentions `mergify {group} {sub}`, which the binary doesn't have"
                f" (it has: {', '.join(sorted(commands[group]))})"
            )
    return problems


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__.splitlines()[2], file=sys.stderr)
        return 2
    commands = native_commands(sys.argv[1])
    skills = sorted(SKILLS.glob("*/SKILL.md"))
    if not skills:
        print(f"::error::no skills found under {SKILLS}")
        return 1
    problems = [p for skill in skills for p in check_skill(skill, commands)]
    for problem in problems:
        print(f"::error::{problem}")
    if problems:
        return 1
    print(f"ok: {len(skills)} skills checked against {sum(map(len, commands.values()))} commands")
    return 0


if __name__ == "__main__":
    sys.exit(main())
