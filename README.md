# Mergify CLI

Mergify CLI is a command-line tool for managing stacked pull requests, CI
pipelines, merge queues, scheduled freezes, and Mergify configuration on
GitHub.

## Features

### Stacked Pull Requests (`mergify stack`)

Create and manage stacked pull requests to break down large changes into
smaller, reviewable pieces. Each commit in a stack becomes its own PR, and
the CLI handles creation, updates, rebasing, and synchronization.
[Documentation](https://docs.mergify.com/stacks/)

| Command | Description |
|---------|-------------|
| `mergify stack new NAME` | Create a new stack branch |
| `mergify stack push` | Push commits and create/update PRs for the stack |
| `mergify stack list` | List commits and their associated PRs (with CI/review status) |
| `mergify stack sync` | Fetch trunk, remove merged commits, rebase |
| `mergify stack edit [COMMIT]` | Interactive edit of the stack history |
| `mergify stack note [COMMIT]` | Attach a "why this commit was amended" note to a commit |
| `mergify stack reorder C A B` | Reorder commits in the stack |
| `mergify stack move X before Y` | Move a commit within the stack |
| `mergify stack checkout` | Check out a stack from a remote repository |
| `mergify stack open [COMMIT]` | Open a PR from the stack in the browser |
| `mergify stack hooks` | Show git hooks status and manage installation |
| `mergify stack setup` | Configure git hooks (alias for `hooks --setup`) |

### CI Insights (`mergify ci`)

Upload JUnit test results, evaluate quarantine status for flaky tests, detect
git references, manage CI scopes for selective testing, and retrieve merge queue
metadata.
[Documentation](https://docs.mergify.com/ci-insights/)

| Command | Description |
|---------|-------------|
| `mergify ci junit-process FILES...` | Upload JUnit XML reports and evaluate quarantine |
| `mergify ci git-refs` | Detect base/head git references for the current PR |
| `mergify ci scopes` | Detect CI scopes impacted by changed files |
| `mergify ci scopes-send` | Send scopes tied to a pull request to Mergify |
| `mergify ci queue-info` | Output merge queue batch metadata |

### Merge Queue (`mergify queue`)

Monitor and manage the Mergify merge queue: view queue status, inspect
individual PRs, and pause/unpause the queue.
[Documentation](https://docs.mergify.com/merge-queue/)

| Command | Description |
|---------|-------------|
| `mergify queue status` | Show merge queue status (batches, waiting PRs) |
| `mergify queue show PR_NUMBER` | Detailed state of a PR in the queue |
| `mergify queue pause --reason "..."` | Pause the merge queue |
| `mergify queue unpause` | Resume the merge queue |

### Scheduled Freezes (`mergify freeze`)

Create and manage scheduled freezes to temporarily halt merging of pull
requests matching specific conditions. Supports time windows, matching
conditions, and exclusions.

| Command | Description |
|---------|-------------|
| `mergify freeze list` | List all scheduled freezes |
| `mergify freeze create` | Create a new scheduled freeze |
| `mergify freeze update FREEZE_ID` | Update an existing freeze |
| `mergify freeze delete FREEZE_ID` | Delete a freeze |

### Configuration Management (`mergify config`)

Validate Mergify configuration files against the official schema and simulate
what Mergify would do on a specific pull request.

| Command | Description |
|---------|-------------|
| `mergify config validate` | Validate the configuration file against the schema |
| `mergify config simulate PR_URL` | Simulate Mergify actions on a pull request |

## Installation

```shell
pip install mergify-cli
```

## Usage

```shell
# Show all available commands
mergify --help

# Stacked pull requests
mergify stack new feat/my-feature    # Create a new stack
mergify stack push                   # Push and create/update PRs
mergify stack list                   # Show stack status
mergify stack sync                   # Sync with upstream
mergify stack checkout my-feature    # Checkout an existing stack from GitHub

# CI insights
mergify ci junit-process results.xml # Upload test results + quarantine
mergify ci scopes                    # Detect impacted scopes
mergify ci git-refs                  # Detect base/head refs
mergify ci git-refs --format=shell   # Emit MERGIFY_GIT_REFS_* vars for `eval`
mergify ci git-refs --format=json    # Emit single-line JSON for jq

# Merge queue
mergify queue status                 # View queue state
mergify queue show 123               # Inspect a PR in the queue
mergify queue pause --reason "..."   # Pause merges
mergify queue unpause                # Resume merges

# Scheduled freezes
mergify freeze list                  # List freezes
mergify freeze create --reason "..." --timezone UTC   # Create a freeze
mergify freeze delete FREEZE_ID      # Remove a freeze

# Configuration
mergify config validate              # Validate .mergify.yml
mergify config simulate PR_URL       # Simulate actions on a PR
```

## AI Agent Skills

Mergify CLI provides AI skills for managing stacked PRs and Git workflows,
compatible with [Claude Code](https://docs.anthropic.com/en/docs/claude-code),
[Cursor](https://cursor.sh), and [many other AI
agents](https://skills.sh).

### Install via npx (all agents)

```shell
npx skills add Mergifyio/mergify-cli
```

### Install as a Claude Code plugin

```shell
/plugin marketplace add Mergifyio/mergify-cli
/plugin install mergify
```

## Exit Codes

The CLI uses structured exit codes so scripts can distinguish failure modes
without parsing stderr:

| Code | Name | Meaning |
|------|------|---------|
| 0 | `SUCCESS` | Command completed successfully |
| 1 | `GENERIC_ERROR` | Unclassified error |
| 2 | *(Click)* | Invalid usage or bad arguments |
| 3 | `STACK_NOT_FOUND` | Stack, branch, or commit not found |
| 4 | `CONFLICT` | Rebase conflict |
| 5 | `GITHUB_API_ERROR` | GitHub API failure |
| 6 | `MERGIFY_API_ERROR` | Mergify API failure |
| 7 | `INVALID_STATE` | Invalid state (e.g. branch targets itself, ambiguous commit) |
| 8 | `CONFIGURATION_ERROR` | Configuration validation failed |

Example usage in a script:

```bash
mergify stack push
case $? in
  0) echo "Success" ;;
  3) echo "Not in a stack" ;;
  4) echo "Rebase conflict — resolve and retry" ;;
  5) echo "GitHub API error — check auth" ;;
  *) echo "Failed with code $?" ;;
esac
```

## Contributing

We welcome and appreciate contributions from the open-source community to make
this project better. Whether you're a developer, designer, tester, or just
someone with a good idea, we encourage you to get involved.

## License

This project is licensed under the Apache License 2.0 - see the
[LICENSE](LICENSE) file for details.
