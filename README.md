# Mergify CLI

Command-line tool for [Mergify](https://mergify.com): stacked pull requests,
CI insights, merge queue, scheduled freezes, and configuration management.

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

Run `mergify --help` to list commands and `mergify <command> --help` for
details. See the [CLI docs](https://docs.mergify.com/cli/) for authentication
and global options (`--token`, `--repository`, `--api-url`).

## Commands

- **`mergify stack`** — Create and manage stacked pull requests.
  [Docs](https://docs.mergify.com/stacks/)
- **`mergify ci`** — Upload JUnit results, evaluate quarantine, detect git
  refs and CI scopes.
  [Docs](https://docs.mergify.com/ci-insights/)
- **`mergify tests`** — Inspect test health and manage quarantine for tests
  tracked by Mergify CI Insights (`mergify tests show NAME...`,
  `mergify tests quarantines add NAME`, `mergify tests quarantines remove NAME`).
  [Docs](https://docs.mergify.com/ci-insights/)
- **`mergify queue`** — Monitor and manage the Mergify merge queue.
  [Docs](https://docs.mergify.com/merge-queue/)
- **`mergify freeze`** — Create and manage scheduled merge freezes.
  [Docs](https://docs.mergify.com/merge-protections/freeze/)
- **`mergify config`** — Validate and simulate Mergify configuration.
  [Docs](https://docs.mergify.com/configuration/file-format/#validating-with-the-cli)

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

## Contributing

Contributions are welcome — open an issue or pull request.

## License

Apache License 2.0 — see [LICENSE](LICENSE).
