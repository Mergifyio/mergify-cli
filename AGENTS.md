# Agent Instructions

This repository is the public home of the `mergify` CLI. It doesn't hold the
CLI's source: Mergify develops, builds and releases the CLI elsewhere, along
with the files this repository publishes.

## Published files: don't edit them here

These are copied from the CLI's source as of the Latest release, by a pull
request from `mergify-ci-bot` (branch `public-files-sync`), opened whenever
they differ:

| Here | What it is |
|---|---|
| `README.md` | the CLI's user documentation (also its PyPI page) |
| `install.sh` | the installer, served from `main` to `curl … \| sh` |
| `skills/` | the agent skills (`npx skills add Mergifyio/mergify-cli`) |
| `.claude-plugin/`, `.codex-plugin/`, `assets/` | the plugin manifests and icon |
| `LICENSE` | the license covering the CLI and these files |

A change made to them here is overwritten by the next sync, so CI's
`synced-files` check fails any pull request that edits them, unless it comes
from `mergify-ci-bot`. Suggestions are welcome as issues.

## Owned here

- `.github/workflows/ci.yaml`, `.mergify.yml`, `renovate.json` and this `AGENTS.md`.
- The releases, published by `mergify-ci-bot`, never by hand. A release carrying
  the CLI binaries is Latest, which install.sh, `mergify self-update`, the
  Homebrew tap and the docs' CLI reference all follow.
- The issue tracker.

## Checks

The published files are checked in the CLI's source before each release,
against the binary that release ships, so they aren't checked again here. CI
(`.github/workflows/ci.yaml`, gated by `ci-gate`) runs:

- `synced-files`: the published files only change through the sync.
