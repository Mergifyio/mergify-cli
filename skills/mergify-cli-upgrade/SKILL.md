---
name: mergify-cli-upgrade
description: Upgrade the mergify CLI when it is older than a Mergify skill. Use when `mergify` rejects a command or flag a skill names, or when asked to check, update or upgrade the mergify CLI. Triggers on unrecognized subcommand, unexpected argument, outdated CLI, upgrade mergify, update mergify, self-update.
---

# Upgrade the Mergify CLI

The skills follow the latest CLI release. When `mergify` rejects a command or
flag a skill names (`unrecognized subcommand`, `unexpected argument`), the
installed CLI is probably older than the skill.

## 1. Compare versions

`mergify --version` prints the installed version. The
[release notes](https://github.com/Mergifyio/mergify-cli/releases) give the
latest one and the release that added the command. A CLI recent enough to have
`mergify self-update --check` prints both versions.

## 2. Ask the user

Upgrading replaces a binary on the user's machine: ask before you do it.

## 3. Upgrade through the channel that installed it

- Homebrew (`command -v mergify` is under `/opt/homebrew/` or a `Cellar/`):
  `brew upgrade mergify-cli`
- uv (`uv tool list` names it): `uv tool upgrade mergify-cli`
- pipx (`pipx list` names it): `pipx upgrade mergify-cli`
- pip (`pip show mergify-cli` finds it): `pip install --upgrade mergify-cli`
- Install script or manual download: `mergify self-update`

## 4. Run the command again

If the upgraded CLI still rejects it, the skill names a command no release has
yet: tell the user rather than guessing another one.
