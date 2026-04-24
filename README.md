# Mergify CLI

Command-line tool for [Mergify](https://mergify.com): stacked pull requests,
CI insights, merge queue, scheduled freezes, and configuration management.

## Installation

```shell
uv tool install mergify-cli
# or
pipx install mergify-cli
```

Run `mergify --help` to list commands and `mergify <command> --help` for
details. See the [CLI docs](https://docs.mergify.com/cli/) for authentication
and global options (`--token`, `--repository`, `--api-url`).

## Commands

- **`mergify stack`** — Create and manage stacked pull requests.
  [Docs](https://docs.mergify.com/stacks/)
- **`mergify ci`** — Upload JUnit results, evaluate quarantine, detect git
  refs and CI scopes.
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
