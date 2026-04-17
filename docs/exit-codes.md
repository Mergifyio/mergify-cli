# Mergify CLI Exit Codes

This document is the authoritative contract for exit codes across the CLI.
It maps each (command, failure mode) pair to a specific `ExitCode`. The
contract is enforced by `mergify_cli/tests/test_exit_code_contract.py` —
every row in the tables below has a corresponding test case.

## Exit code catalog

| Code | Name                  | Meaning                                                     |
|------|-----------------------|-------------------------------------------------------------|
| 0    | `SUCCESS`             | Command completed successfully.                             |
| 1    | `GENERIC_ERROR`       | Unclassified error or input-data problem without a specific code. |
| 2    | *(Click)*             | Invalid CLI usage or bad argument (`click.BadParameter`, `click.UsageError`). |
| 3    | `STACK_NOT_FOUND`     | Stack, branch, or commit not found.                         |
| 4    | `CONFLICT`            | Rebase/merge conflict encountered.                          |
| 5    | `GITHUB_API_ERROR`    | GitHub API failure (HTTP error against github.com).         |
| 6    | `MERGIFY_API_ERROR`   | Mergify API failure (HTTP error against Mergify).           |
| 7    | `INVALID_STATE`       | Invalid state (e.g., branch targets itself, ambiguous commit). |
| 8    | `CONFIGURATION_ERROR` | Configuration validation failed.                            |

## Contract by command

### `mergify config validate`

| Failure mode                                | ExitCode                |
|---------------------------------------------|-------------------------|
| Configuration file not found (autodetect)   | `CONFIGURATION_ERROR`   |
| Configuration file does not exist (path)    | `CONFIGURATION_ERROR`   |
| Invalid YAML                                | `CONFIGURATION_ERROR`   |
| OS/IO error reading config                  | `CONFIGURATION_ERROR`   |
| Failed to fetch validation schema (HTTP)    | `MERGIFY_API_ERROR`     |
| Failed to parse validation schema           | `GENERIC_ERROR`         |
| Schema validation produced errors           | `CONFIGURATION_ERROR`   |
| Success                                     | `SUCCESS`               |

### `mergify config simulate PR_URL`

| Failure mode                                | ExitCode                |
|---------------------------------------------|-------------------------|
| Invalid PR URL                              | 2 *(click.BadParameter)* |
| Configuration file not found                | `CONFIGURATION_ERROR`   |
| HTTP error against Mergify                  | `MERGIFY_API_ERROR`     |
| Success                                     | `SUCCESS`               |

### `mergify ci scopes`

| Failure mode                                | ExitCode                |
|---------------------------------------------|-------------------------|
| Mergify configuration file not found        | `CONFIGURATION_ERROR`   |
| Config file does not exist                  | `CONFIGURATION_ERROR`   |
| ScopesError during detection                | `CONFIGURATION_ERROR`   |
| Success                                     | `SUCCESS`               |

### `mergify ci scopes-send`

| Failure mode                                | ExitCode                |
|---------------------------------------------|-------------------------|
| ScopesError loading file                    | `CONFIGURATION_ERROR`   |
| HTTP error against Mergify                  | `MERGIFY_API_ERROR`     |
| Success                                     | `SUCCESS`               |

### `mergify ci queue-info`

| Failure mode                                | ExitCode                |
|---------------------------------------------|-------------------------|
| Not running in merge queue context          | `INVALID_STATE`         |
| Success                                     | `SUCCESS`               |

### `mergify ci junit-process`

| Failure mode                                | ExitCode                |
|---------------------------------------------|-------------------------|
| Invalid JUnit XML                           | `GENERIC_ERROR`         |
| No spans in JUnit files                     | `GENERIC_ERROR`         |
| No test cases in JUnit files                | `GENERIC_ERROR`         |
| Silent failure (test runner non-zero, no failures reported) | `GENERIC_ERROR` |
| Non-quarantined failures present            | `GENERIC_ERROR`         |
| All tests passed or all failures quarantined | `SUCCESS`              |

### `mergify stack new NAME`

| Failure mode                                | ExitCode                |
|---------------------------------------------|-------------------------|
| Branch already exists                       | `STACK_NOT_FOUND`       |
| Git command failure                         | `GENERIC_ERROR`         |
| Success                                     | `SUCCESS`               |

### `mergify stack push`

| Failure mode                                | ExitCode                |
|---------------------------------------------|-------------------------|
| Branch is trunk                             | `INVALID_STATE`         |
| Branch targets itself                       | `INVALID_STATE`         |
| No commits in stack                         | `STACK_NOT_FOUND`       |
| Success (nothing to push)                   | `SUCCESS`               |
| Success                                     | `SUCCESS`               |

### `mergify stack list`

| Failure mode                                | ExitCode                |
|---------------------------------------------|-------------------------|
| Ambiguous state                             | `INVALID_STATE`         |
| Invalid state                               | `INVALID_STATE`         |
| Stack not found                             | `STACK_NOT_FOUND`       |
| Success                                     | `SUCCESS`               |

### `mergify stack sync`

| Failure mode                                | ExitCode                |
|---------------------------------------------|-------------------------|
| Invalid state                               | `INVALID_STATE`         |
| Stack not found                             | `STACK_NOT_FOUND`       |
| Success                                     | `SUCCESS`               |

### `mergify stack open [COMMIT]`

| Failure mode                                | ExitCode                |
|---------------------------------------------|-------------------------|
| Commit not found / not in stack             | `STACK_NOT_FOUND`       |
| Success (no PR to open — returns 0)         | `SUCCESS`               |
| Success                                     | `SUCCESS`               |

### `mergify stack checkout`

| Failure mode                                | ExitCode                |
|---------------------------------------------|-------------------------|
| Invalid state                               | `INVALID_STATE`         |
| Already up to date                          | `SUCCESS`               |
| Success                                     | `SUCCESS`               |

### `mergify stack reorder`

| Failure mode                                | ExitCode                |
|---------------------------------------------|-------------------------|
| Commit not found                            | `STACK_NOT_FOUND`       |
| Ambiguous / invalid input                   | `INVALID_STATE`         |
| Rebase conflict                             | `CONFLICT`              |
| Success                                     | `SUCCESS`               |

### `mergify stack move`

| Failure mode                                | ExitCode                |
|---------------------------------------------|-------------------------|
| Invalid move (3 variants)                   | `INVALID_STATE`         |
| Success                                     | `SUCCESS`               |

### `mergify queue pause`

| Failure mode                                | ExitCode                |
|---------------------------------------------|-------------------------|
| Invalid state                               | `INVALID_STATE`         |
| Mergify API error                           | `MERGIFY_API_ERROR`     |
| Success                                     | `SUCCESS`               |

### `mergify queue unpause`

| Failure mode                                | ExitCode                |
|---------------------------------------------|-------------------------|
| Mergify API error                           | `MERGIFY_API_ERROR`     |
| Success                                     | `SUCCESS`               |

## Top-level fallbacks

These apply to any command.

| Failure mode                                | ExitCode                |
|---------------------------------------------|-------------------------|
| HTTP error against github.com               | `GITHUB_API_ERROR`      |
| HTTP error against Mergify API              | `MERGIFY_API_ERROR`     |
| Subprocess (git) command failed             | `GENERIC_ERROR`         |
| Uncaught `MergifyError` (no explicit code)  | `GENERIC_ERROR`         |
| Click usage / parameter errors              | 2                       |
