# Mergify CLI

[![CI](https://github.com/Mergifyio/mergify-cli/actions/workflows/ci.yaml/badge.svg)](https://github.com/Mergifyio/mergify-cli/actions/workflows/ci.yaml)
[![Latest release](https://img.shields.io/github/v/release/Mergifyio/mergify-cli?logo=github&label=release)](https://github.com/Mergifyio/mergify-cli/releases/latest)
[![Documentation](https://img.shields.io/badge/docs-mergify.com-7c3aed)](https://docs.mergify.com/cli/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

Drive [Mergify](https://mergify.com) from your terminal and CI pipelines:
stacked pull requests, the merge queue, CI Insights, scheduled freezes, and
configuration — all from a single self-contained binary that reuses your
existing GitHub (`gh`) login.

```shell
mergify stack push          # turn your local commits into stacked PRs
mergify queue status        # inspect the merge queue
mergify ci junit-process report.xml   # upload test results to CI Insights
```

- **One static binary.** No runtime, no dependencies — drop it on a developer
  laptop or a CI runner and go.
- **Zero-config auth.** Picks up `gh auth token` automatically; override with
  env vars or flags when scripting.
- **Built for pipelines.** Logs to stderr, structured `--json` output on read
  commands, and stable [exit codes](#exit-codes) for scripts and runbooks.
- **Cross-platform.** Linux, macOS (x86_64 + aarch64), and Windows.

## Installation

### Homebrew (recommended for macOS)

```shell
brew install mergifyio/tap/mergify-cli
```

The fully-qualified name taps and installs in one step. Upgrade with
`brew upgrade mergify-cli` — not `mergify self-update`, which overwrites the
Homebrew-managed binary. See the [tap](https://github.com/Mergifyio/homebrew-tap)
for tap-trust and short-name details.

### Install script (recommended for Linux; also macOS — x86_64 and aarch64)

```shell
curl -fsSL https://raw.githubusercontent.com/Mergifyio/mergify-cli/main/install.sh | sh
```

Installs to `~/.local/bin/mergify`. Override with `MERGIFY_INSTALL_DIR=/usr/local/bin`
or pin a version with `MERGIFY_VERSION=<version>`. Upgrade with `mergify self-update`.

### Manual download (Windows, or to bypass the script)

Grab the matching archive from the
[latest release](https://github.com/Mergifyio/mergify-cli/releases/latest):

- **Windows** — download `mergify-<version>-x86_64-pc-windows-msvc.zip`,
  extract it, and put `mergify.exe` anywhere on your `PATH`.
- **Linux / macOS** — download `mergify-<version>-<target>.tar.gz` (e.g.
  `mergify-2026.4.23.1-aarch64-apple-darwin.tar.gz`), extract with `tar -xzf`,
  and put the resulting `mergify` binary anywhere on your `PATH`.

Verify against `SHA256SUMS` from the same release if you care.

## Authentication

Most commands talk to the Mergify and GitHub APIs and need a token. The CLI
resolves credentials and target repository in this order, so an authenticated
`gh` is usually all you need:

| What | `--flag` | then env | then |
| --- | --- | --- | --- |
| **Token** | `--token` / `-t` | `MERGIFY_TOKEN`, `GITHUB_TOKEN` | `gh auth token` |
| **Repository** | `--repository` / `-r` | `GITHUB_REPOSITORY` | `git remote` (`origin`) |
| **API URL** | `--api-url` / `-u` | `MERGIFY_API_URL` | `https://api.mergify.com` |

See the [authentication guide](https://docs.mergify.com/cli/usage) for details.

## Quick start

```shell
# Stacked pull requests — one PR per commit, kept in sync
mergify stack setup                # once per repo: install the git hooks the stack needs
mergify stack push                 # push commits and create/update their PRs
mergify stack list                 # show the stack and its PR status
mergify stack sync                 # rebase the stack onto its trunk

# Merge queue
mergify queue status               # current queue state for the repo
mergify queue status --json        # same, as machine-readable JSON

# CI Insights — from inside your pipeline
mergify ci junit-process report.xml --test-language python

# Configuration
mergify config validate            # check .mergify.yml against the schema
```

Run `mergify --help` for the full command list and `mergify <command> --help`
for any command's flags.

## Commands

Every command group maps to a section of the
[CLI reference](https://docs.mergify.com/cli/).

- **`mergify stack`** — Create and maintain stacked pull requests.
  [Docs](https://docs.mergify.com/stacks/)
- **`mergify queue`** — Inspect and control the merge queue.
  [Docs](https://docs.mergify.com/merge-queue/)
- **`mergify ci`** — Send JUnit results and pull request scopes from any CI
  provider. [Docs](https://docs.mergify.com/ci-insights/)
- **`mergify tests`** — Look up test health and manage the flaky-test
  quarantine. [Docs](https://docs.mergify.com/ci-insights/)
- **`mergify freeze`** — Schedule merge freezes for release windows and
  maintenance. [Docs](https://docs.mergify.com/merge-protections/freeze/)
- **`mergify config`** — Validate your configuration and simulate actions
  before you merge. [Docs](https://docs.mergify.com/configuration/file-format/#validating-with-the-cli)
- **`mergify self-update`** — Update the CLI to the latest release.
- **`mergify completions <shell>`** — Print a shell completion script
  ([see below](#shell-completions)).

Run `mergify <command> --help` for a group's subcommands and flags.

## Shell completions

Generate a completion script for your shell — `bash`, `zsh`, `fish`,
`elvish`, or `powershell`:

```shell
# zsh — write to a directory on your $fpath
mergify completions zsh > ~/.zfunc/_mergify

# bash — load in your current session (add to ~/.bashrc to persist)
source <(mergify completions bash)

# fish
mergify completions fish > ~/.config/fish/completions/mergify.fish
```

## Global options

These are accepted on every command:

| Flag | Description |
| --- | --- |
| `-v`, `--verbose` | Increase log verbosity: `-v` info, `-vv` debug, `-vvv` trace. Logs go to stderr so stdout stays pipeable. |
| `--debug` | Shorthand for at least debug-level logging (like `-vv`). |
| `--color <auto\|always\|never>` | When to colorize terminal output. |

## Environment variables

| Variable | Effect |
| --- | --- |
| `MERGIFY_TOKEN`, `GITHUB_TOKEN` | API token (falls back to `gh auth token`). |
| `GITHUB_REPOSITORY` | Default `owner/repo` when `--repository` is omitted. |
| `MERGIFY_API_URL` | API base URL (default `https://api.mergify.com`). |
| `RUST_LOG` | Fine-grained log filtering; overrides `--verbose`. |
| `NO_COLOR` | Disable colored output. |
| `MERGIFY_INSTALL_DIR`, `MERGIFY_VERSION` | Install-script target directory / pinned version. |

## Exit codes

Commands return stable exit codes so scripts and runbooks can branch on them:

| Code | Meaning |
| --- | --- |
| `0` | Success. |
| `1` | Unclassified runtime failure (I/O error, bug, or captured panic). |
| `2` | Argument parsing / usage error. |
| `3` | Stack, branch, or commit not found. |
| `4` | Rebase or merge conflict. |
| `5` | GitHub API request failed. |
| `6` | Mergify API request failed. |
| `7` | CLI invariant violated (e.g. run outside a valid context). |
| `8` | Configuration missing, unparseable, or failing validation. |

## AI Agent Skills

Mergify CLI provides AI skills for managing stacked PRs and Git workflows,
compatible with [Claude Code](https://docs.anthropic.com/en/docs/claude-code),
[Cursor](https://cursor.sh), and [many other AI agents](https://skills.sh).

Install via npx (all agents):

```shell
npx skills add Mergifyio/mergify-cli
```

Install as a Claude Code plugin:

```shell
/plugin marketplace add Mergifyio/mergify-cli
/plugin install mergify
```

## Documentation

Full reference and guides live at
**[docs.mergify.com/cli](https://docs.mergify.com/cli/)**.

## Contributing

Contributions are welcome — open an
[issue](https://github.com/Mergifyio/mergify-cli/issues) or a pull request.
The workspace is a Rust monorepo; see [AGENTS.md](AGENTS.md) for the crate
layout, build, and test workflow.

## License

Apache License 2.0 — see [LICENSE](LICENSE).
