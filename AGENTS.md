# Agent Instructions

This repository is the public home of the `mergify` CLI. It doesn't hold the
CLI's source: the CLI is developed, built and released from Mergify's private
monorepo, and so are the files this repository publishes.

## Published files: don't edit them here

These are copied from the CLI's source (`cli/` in the private monorepo) as of
the Latest release, by a pull request from `mergify-ci-bot` (branch
`public-files-sync`) that ci-bot's `public-releases-sync` opens whenever they
differ:

| Here | What it is |
|---|---|
| `README.md` | the CLI's user documentation (also its PyPI page) |
| `install.sh` | the installer, served from `main` to `curl … \| sh` |
| `skills/` | the agent skills (`npx skills add Mergifyio/mergify-cli`) |
| `.claude-plugin/`, `.codex-plugin/`, `assets/` | the plugin manifests and icon |
| `scripts/check-skills.py` | the skill check CI runs |

A change made to them here is overwritten by the next sync, so CI's
`synced-files` check fails any pull request that edits them, unless it comes
from `mergify-ci-bot`. Suggestions are welcome as issues, or as pull requests
that Mergify carries over to the CLI's source.

## Owned here

- `.github/workflows/ci.yaml`, `.mergify.yml`, `renovate.json`,
  `_typos.toml`, this `AGENTS.md` and `LICENSE`.
- The releases, written by ci-bot's mirror, never by hand. A release carrying
  the CLI binaries is Latest, which install.sh, `mergify self-update`, the
  Homebrew tap and the docs' CLI reference all follow.
- The issue tracker.

## Checks

CI (`.github/workflows/ci.yaml`, gated by `ci-gate`) runs against the latest
release, since nothing is built here:

- `skills`: `scripts/check-skills.py` checks every skill's front matter and
  that every `mergify <group> <subcommand>` it mentions exists in the latest
  release's binary.
- `install-script-release`: shellcheck, then install.sh end to end against
  the latest release's real files served from a local fixture
  (`MERGIFY_BASE_URL`), on Linux, macOS and Windows, including the tampered
  `SHA256SUMS` cases.
- `synced-files`: the published files only change through the sync.
- `typos`, with the allow-list in `_typos.toml`.
