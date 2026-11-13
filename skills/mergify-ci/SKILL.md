---
name: mergify-ci
description: Use Mergify CI commands to upload JUnit test results, detect git references, manage CI scopes, and retrieve merge queue metadata. ALWAYS use this skill when running `mergify ci` or `mergify tests` commands, uploading test results to Mergify, managing Mergify test quarantines, detecting Mergify CI scopes, or reading Mergify merge queue CI context. Triggers on mergify ci, mergify tests, junit-process, Mergify Test Engine, CI Insights, Mergify quarantine, Mergify scopes, scopes-send, git-refs, queue-info.
---

# Mergify CI Commands

## Overview

The `mergify ci` command group provides tools for CI pipelines: uploading JUnit test results to Mergify Test Engine, detecting git references for diff-based operations, managing CI scopes for selective testing, and retrieving merge queue batch metadata.

## Commands

```bash
mergify ci junit-process FILES...         # Upload JUnit XML + evaluate quarantine (primary command)
mergify ci junit-upload FILES...          # (Deprecated) Use junit-process instead
mergify ci git-refs                       # Detect base/head git references for the current PR
mergify ci scopes --config PATH           # Detect scopes impacted by changed files
mergify ci scopes-send -s SCOPE           # Report scopes against the pull request's head commit
mergify ci queue-info                     # Output the current build's merge queue batch metadata (from the git note)
mergify tests show NAME...                # Look up tests by name and print health, ratios, last failure
mergify tests runs                        # List a commit's test runs, by the job they arrived under
mergify tests quarantines add NAME        # Add a test to the Test Engine quarantine
mergify tests quarantines remove NAME     # Remove a test from the Test Engine quarantine
mergify tests quarantines get NAME        # Print a single quarantine by test name or id
mergify tests quarantines list            # List the tests currently in the Test Engine quarantine
```

## Authentication

Two classes of Mergify application key, both minted in the dashboard. Each
reaches its own list of endpoints, not a subset of the other's: of what this
CLI calls, the two lists share only the quarantine list.

| Key | Reaches |
| --- | --- |
| `ci` | What a CI job does: trace upload, `scopes-send`, quarantine *evaluation* (`junit-process`) and the quarantine *list*. |
| `admin` | Reading test health, the quarantine *list*, and mutating the quarantine. Not trace upload, not `scopes-send`. |

Trace upload and `scopes-send` take a `ci` key only: an `admin` key or a
user credential gets a `403`. Every other endpoint below that refuses a `ci`
key accepts a user credential (`mergify auth login`) instead. `GITHUB_TOKEN`
inside GitHub Actions is not a Mergify credential at all, so do not reach for
it to clear one of these `403`s.

The split follows the endpoint each command calls: reads of test health and
writes to the quarantine are a person inspecting or overriding a repository,
not something a pipeline does, so they are outside what a `ci` key carries;
trace upload and scope reports are what a pipeline sends, so they are outside
what an `admin` key carries. Per command:

| Command | `ci` key | `admin` key |
| --- | --- | --- |
| `ci junit-process`, `ci junit-upload` | yes | quarantine evaluation only — the upload gets a `403`, reported as a rejected upload (the exit code is unaffected) |
| `ci scopes-send` | yes | **no** — `403` |
| `tests runs` | yes | yes (and `mergify auth login`) |
| `tests quarantines list`, `tests quarantines get` | yes — both read the quarantine list | yes |
| `tests show` | **no** — `403` | yes |
| `tests quarantines add`, `tests quarantines remove` | **no** — `403` | yes |

`ci git-refs`, `ci scopes` and `ci queue-info` are evaluated locally and need
no token at all.

## JUnit Processing (`junit-process`)

The primary CI command. Parses JUnit XML reports, checks quarantine status for failing tests, uploads results to Mergify Test Engine, and determines the final CI exit code.

```bash
mergify ci junit-process \
  --repository owner/repo \
  --tests-target-branch main \
  path/to/junit-results.xml
```

The key is read from `MERGIFY_TOKEN`: set it in the job's environment from a secret rather than passing `--token`, which would put it in the process list.

`FILES` can be individual paths or quoted glob patterns (e.g. `'reports/**/*.xml'`). Always quote the pattern so Mergify expands it rather than the shell — this is the recommended approach for large, sharded test suites.

**Key options:**
- `--token` / `-t` (env: `MERGIFY_TOKEN`) -- CI Insights application key
- `--repository` / `-r` -- Repository full name (auto-detected from the CI provider, then the git remote)
- `--tests-target-branch` / `-b` -- Branch used for quarantine evaluation. Auto-detected per CI provider: GitHub Actions (`GITHUB_BASE_REF` → `GITHUB_HEAD_REF` → `GITHUB_REF_NAME`), Buildkite (`BUILDKITE_PULL_REQUEST_BASE_BRANCH` → `BUILDKITE_BRANCH`), CircleCI (the base of the open pull request whose head is `CIRCLE_SHA1`, as Mergify knows it, unless `CIRCLE_BRANCH` is the repository's default branch → `CIRCLE_BRANCH`), Jenkins (`CHANGE_TARGET` → `GIT_BRANCH`), then the checked-out branch. On a branch of GitHub's merge queue (`gh-readonly-queue/<base>/pr-…`), whatever the provider, the branch it merges into. Without one, quarantine is off for the run: the results are still uploaded, and every failure blocks.
- `--api-url` / `-u` (env: `MERGIFY_API_URL`) -- Mergify API URL (default: `https://api.mergify.com`)
- `--test-framework` -- Test framework name (optional metadata)
- `--test-language` -- Test language (optional metadata)
- `--test-exit-code` / `-e` (env: `MERGIFY_TEST_EXIT_CODE`) -- Exit code of the test runner, used to detect silent failures where the runner crashed but the JUnit report appears clean
- `MERGIFY_TRACEPARENT` (env only) -- A W3C `traceparent` to put the uploaded test session inside a trace the caller already started; a malformed value is ignored and the session starts its own trace

**Behavior:**
1. Parses JUnit XML files into test spans
2. Checks quarantine status for failing tests against the Mergify API
3. Checks the spans against the schema Mergify publishes of what it can ingest (`https://docs.mergify.com/mergify-ci-traces-schema.json`), then uploads them to Mergify Test Engine. An upload Mergify would drop (for example one without a branch, `vcs.ref.head.name`) is not sent: the report lists what is missing, and the run is reported as a rejected upload. When the schema cannot be fetched, the spans are uploaded unchecked
4. Prints a summary: tests run, failures, quarantined vs blocking
5. Exits with code 0 if all failures are quarantined, code 1 if any are blocking
6. If `--test-exit-code` is non-zero but no test failures are found, exits with code 1 (silent failure detection)
7. Upload failures never affect the exit code (Mergify-side trouble must not break CI), but they are surfaced: on GitHub Actions the command emits an `::error::` annotation when the upload is rejected (refused by the schema check, or HTTP 4xx except 408/429, e.g. a token without access to the repository, or Test Engine turned off for it: the annotation then gives Mergify's reason) or a `::warning::` annotation for transient errors (5xx, 408, 429, network), and appends `test_results_upload=success|rejected|failed` to `$GITHUB_OUTPUT` so workflows can detect dead ingest programmatically

**GitHub Actions example:**
```yaml
- name: Run tests
  id: tests
  run: pytest --junitxml=results.xml || echo "exit_code=$?" >> "$GITHUB_OUTPUT"

- name: Process test results
  if: always()
  run: |
    mergify ci junit-process \
      --test-exit-code ${{ steps.tests.outputs.exit_code || 0 }} \
      results.xml
  env:
    MERGIFY_TOKEN: ${{ secrets.MERGIFY_TOKEN }}
```

## Git References (`git-refs`)

Detects the base and head git references for the current pull request context. Writes results to `GITHUB_OUTPUT` when running in GitHub Actions, and to Buildkite meta-data (`mergify-ci.base`, `mergify-ci.head`, `mergify-ci.source`) when running in Buildkite.

```bash
mergify ci git-refs
# Output:
# Base: abc1234
# Head: def5678
```

**Output formats (`--format`):**
- `text` (default) — human-readable `Base:` / `Head:` lines
- `shell` — `MERGIFY_GIT_REFS_{BASE,HEAD,SOURCE}=...` lines suitable for `eval`, with POSIX-safe shell quoting. When base can't be detected, `MERGIFY_GIT_REFS_BASE=''`.
- `json` — single-line JSON object with `base`, `head`, `source` keys. `base` may be `null` when it can't be detected (e.g., `workflow_dispatch` events); use `jq -r '.base // ""'` to coalesce to empty string.

```bash
# Consume values in a shell script without parsing (a failure inside
# `eval "$(...)"` would go unnoticed):
refs=$(mergify ci git-refs --format=shell) || exit 1
eval "$refs"
nx show projects --affected \
  --base="$MERGIFY_GIT_REFS_BASE" \
  --head="$MERGIFY_GIT_REFS_HEAD"

# Or with jq:
BASE=$(mergify ci git-refs --format=json | jq -r '.base // ""')
```

Sources detected (in priority order): merge queue context, GitHub pull request event, GitHub push event, Jenkins change request, fallback to last commit.

On a Jenkins change request build (`CHANGE_ID` set by a multibranch pipeline) that is not a merge queue draft (below), the base is the branch the pull request targets (`CHANGE_TARGET`), printed as the commit `origin/<branch>` names, and the head is `HEAD`. Jenkins fetches `origin/<branch>` when it merges the pull request into its target, and checks out the pull request alone otherwise: `git-refs` then fetches the target branch from `origin`, with the history it shares with `HEAD`, and fails when it cannot. That `git fetch` needs the repository's credentials in the step.

The merge queue context is the git note Mergify writes on the merge queue branch head (`refs/notes/mergify/<branch>`). It is read on pull request builds, on GitHub `push` builds of the branch, and on Buildkite and Jenkins builds of the branch, so a merge queue batch a CI builds as a branch, by a `push` trigger or a branch job, rather than as its draft pull request, gets the batch's checking base too. On a Jenkins branch build, the branch is `GIT_BRANCH`, or `BRANCH_NAME` in a scripted pipeline, which does not export `GIT_BRANCH`, and the head is `HEAD`. On a Jenkins change request, the branch is the one it comes from (`CHANGE_BRANCH`), unless that is a fork's (`CHANGE_FORK`), and the head is the commit the note is on: the commit Jenkins checked out (`GIT_COMMIT`, else `HEAD`), or its first parent when Jenkins merged a target that moved on since into the draft. The base, the commit the batch was built on, is fetched from `origin` when a shallow clone lacks it, and `git-refs` fails when it cannot. The note's `git fetch` needs the repository's credentials in the step; without them the build is read as any other change request or branch build. A push that creates a branch has no previous commit, so its base is the default branch.

On a GitHub `push` that did not fast-forward (the event's `forced`), such as a push back to an earlier commit, the head is a commit `git-refs` makes in the local clone: the pushed commit's files, with the base, the commit the push replaced, as its only parent. Its merge base with the base is the base, so the `nx show projects --affected --base --head` call above, `git diff base...head` and the other tools that compare from the merge base see every file the push changed, a rollback's included. `git-refs` fetches the base by its SHA when the clone lacks it, and prints the pushed commit with a warning when it cannot, or out of a clone. No ref names that head and no other clone has it: run `mergify ci git-refs` in the clone that compares, where it makes the same commit, rather than passing its head to another job.

## Scopes (`scopes`)

Detects which CI scopes are impacted by changed files, based on a Mergify configuration file. Used for selective/targeted CI -- only run tests for scopes that have changed files.

```bash
# Detect scopes from changed files
mergify ci scopes --config .mergify.yml

# Detect scopes with explicit base/head
mergify ci scopes --config .mergify.yml --base origin/main --head HEAD

# Write detected scopes to a file
mergify ci scopes --config .mergify.yml --write scopes.json
```

**Key options:**
- `--config` (env: `MERGIFY_CONFIG_PATH`) -- Path to the Mergify YAML config file (auto-detected)
- `--base` -- Base git reference (auto-detected)
- `--head` -- Head git reference (default: HEAD)
- `--write` / `-w` -- Write detected scopes to a JSON file

The config file defines scopes with file patterns. When files change between base and head, matching scopes are identified and written to `GITHUB_OUTPUT` (on GitHub Actions) or to Buildkite meta-data under `mergify-ci.scopes` (on Buildkite), as a JSON map of scope names to `"true"`/`"false"`.

When the base is the `before` commit detected from a GitHub `push` event, the changed files are the ones that differ between it and the pushed commit, compared directly rather than from their merge base: a push back to an earlier commit selects the scopes of the files it reverts. Every other base, an explicit `--base` included, counts what `head` adds on top of it, from their merge base.

On a merge queue batch build (a draft pull request, or a `push` build of the merge queue branch), scopes are not computed from changed files: the command reads the engine's git note for the batch and returns the scopes the merge queue decided (the batch's scopes plus each batched pull request's scopes, including scopes a pull request's CI reported), plus the merge queue scope. A batch the note marks as a barrier (`all_scopes: true`) selects every scope. This also works with a `manual` scopes source. Without a note, or with a note that has no `scopes` list, it falls back to diffing files.

A renamed file counts against **both** of its paths, same as the engine: `git mv critical/guard.txt ignored/guard.txt` touches `critical` and `ignored`.

## Scopes Send (`scopes-send`)

Sends scopes tied to a pull request to the Mergify API. Used when scopes are determined manually or from a file rather than auto-detected.

The report is addressed by the pull request's **head SHA**, so it says which revision it was computed for and a result computed for an older head cannot be taken for the current one. The head is detected from the CI environment (GitHub Actions event payload, `BUILDKITE_COMMIT`, `CIRCLE_SHA1`, or a Jenkins pull request build's checkout); pass `--head-sha` to name it explicitly wherever it is not detected: a GitHub Actions event with no `pull_request` payload (`push`, `workflow_run`, ...), any other CI. A Jenkins multibranch pull request build needs no flag: the number comes from `CHANGE_ID`, and the head is the commit checked out (`GIT_COMMIT`, else `HEAD`) or, when the GitHub Branch Source plugin merges the pull request into its target branch, the first parent of the merge Jenkins made. No head is detected where git cannot read the checkout, such as a container without its `.git`, nor on an agent that merges with JGit, whose merge Jenkins does not make under its own name: pass `--head-sha` with the pull request's head there, or switch the source's strategy to "The current pull request revision". When no head SHA can be resolved, the command reports against the pull request number alone and warns: that route is deprecated and will be removed. A Mergify deployment that predates the commit endpoint (on-premise before 8.18.0) gets the same pull request route without a warning.

Needs a `ci` key — an `admin` key or a user credential gets a `403`. See
[Authentication](#authentication).

```bash
# Send specific scopes
mergify ci scopes-send -s frontend -s backend -p 123

# Send scopes from a JSON file (produced by `mergify ci scopes --write`)
mergify ci scopes-send --scopes-json scopes.json -p 123

# Send scopes from a plain-text file (one scope per line)
mergify ci scopes-send --scopes-file scopes.txt -p 123

# Declare the PR impacts every scope (merge-queue barrier),
# e.g. a build-system or CI-workflow change
mergify ci scopes-send -s build-system --all -p 123

# Name the revision explicitly wherever it is not detected (the pull
# request head, not the revision a `pull_request` job or a Jenkins
# merge build checked out)
mergify ci scopes-send -s frontend -p 123 --head-sha "$PR_HEAD_SHA"
```

**Key options:**
- `--token` / `-t` (env: `MERGIFY_TOKEN`) -- Mergify `ci` application key
- `--repository` / `-r` -- Repository full name (auto-detected)
- `--pull-request` / `-p` -- Pull request number (auto-detected under GitHub Actions pull request events, Buildkite and Jenkins)
- `--scope` / `-s` -- Scope name (repeatable)
- `--scopes-json` -- JSON file containing scopes (output of `mergify ci scopes --write`)
- `--scopes-file` -- Plain-text file with one scope per line
- `--head-sha` -- Head SHA the scopes were computed for, 40 hexadecimal characters (auto-detected under GitHub Actions pull request events, Buildkite, CircleCI and Jenkins pull request builds)
- `--all` -- Declare the pull request impacts every scope. The merge queue treats it as a barrier: never batched or run in parallel with other pull requests. The concrete scopes are still sent alongside the flag.

## Tests Show (`tests show`)

Looks up tests by name on the repository's default branch and prints their
health, success/failure ratios, and last failure context. The search is a
batch API: pass one or more names (globs supported) and one block per match
is rendered. It is read-only: it exits `0` once it has rendered the matches,
whatever their health. Gate on health by consuming `--json`, not the exit code.

Needs an `admin` key or a user credential (`mergify auth login`) — a `ci` key gets a `403`. See
[Authentication](#authentication).

```bash
# Single test.
mergify tests show -r owner/repo \
  'ApplicationKeys.spec.ts.Permissions › Should not see keys table if not admin'

# Batch with glob, narrowed to one pipeline, JSON for jq.
mergify tests show -r owner/repo \
  --pipeline-name e2e --json \
  '*test_login*' '*test_logout*' \
  | jq '.tests[] | {test_name, health_status}'
```

**Key options:**
- `--repository` / `-r` -- Repository full name (`owner/repo`); auto-detected from the CI environment or the local git remote when omitted.
- `--token` / `-t` (env: `MERGIFY_TOKEN`) -- Mergify credential. Falls back to the credential `mergify auth login` stored. The CLI no longer falls back to `GITHUB_TOKEN` or `gh auth token`.
- `--api-url` / `-u` (env: `MERGIFY_API_URL`) -- API base URL.
- `--pipeline-name`, `--pipeline-name-exclude` -- Restrict / exclude by pipeline.
- `--job-name`, `--job-name-exclude` -- Restrict / exclude by job.
- `--per-page` -- Cap the search result count (1–100, server default 10).
- `--json` -- Emit a single JSON document `{"tests": [...]}` to stdout.

**Exit codes:**
- `0` -- Tests rendered, or no match at all. Health does **not** affect it.
- `6` -- Mergify API error, including the `403` a `ci` key gets here.

## Tests Runs (`tests runs`)

A test result upload that fails leaves the job green: the plugin exits 0
when Mergify refuses the upload, and results sent under the wrong job name
pass unnoticed. `mergify tests runs` lists the test runs Mergify received
for a commit (the `Test run ID: …` a plugin prints in the CI log), one line
per job name they arrived under (`MERGIFY_TEST_JOB_NAME`, else the CI's job
name — on GitHub Actions the job id, not its displayed name): how many runs,
how many arrived whole, framework, plugin or converted report and the
client's version, test counts. Only a run that did not arrive whole gets a
line of its own.

```bash
mergify tests runs                 # The commit under test (in CI, the one results are uploaded for; else HEAD)
mergify tests runs --pr 123        # A pull request's head commit, as the origin remote has it
mergify tests runs --branch main   # A branch's commit on the origin remote
mergify tests runs --sha <SHA>     # A full commit SHA
mergify tests runs --json          # The API response as is, for jq or an agent
```

Each job name reads in one of these states (`test_results` in `--json`):

| State | Meaning |
| --- | --- |
| `results_received` | Every run received so far arrived whole. |
| `upload_error` | A run arrived incomplete or unreadable (results left out, a missing part). |
| `ingesting` | Part of a run is still waiting to be ingested: check again shortly. |

It reads what Test Engine received and nothing else, the same on every CI
provider. So to check a setup, **compare the jobs you configured to upload
with the job names listed**: a name missing means nothing from that job
reached Mergify — open the job in your CI to see whether it ran, and read its
upload output (a plugin that failed before sending, or a refused upload,
shows there). A name you did not expect, or several jobs under one name,
means the job name sent is not the one you meant: set `MERGIFY_TEST_JOB_NAME`.
A repository that refuses every upload (Test Engine turned off for it) says
so above the list (`test_results_ingestion` in `--json`).

Each job reads from its latest CI run: a commit run on push and on its pull
request reads from the later one. A re-run within it shows beside the run it
repeats, counted as a re-run, each run with its attempt. The command exits 0
whatever the states: gate on `--json` if you need to.

## Tests Quarantines Add (`tests quarantines add`)

Adds a test to the repository's Test Engine quarantine, so its failures stop
blocking the CI verdict. Takes a single fully qualified test name; a `--reason`
is required.

A quarantined test keeps running but stops failing CI, so nothing reminds
anyone to fix it. **When you quarantine a test, link the issue tracking its
fix with `--issue-url`**: file the issue first (or find the existing one),
then pass its URL. Mergify shows the link on the quarantine.

Needs an `admin` key or a user credential (`mergify auth login`) — a `ci` key gets a `403`. See
[Authentication](#authentication).

```bash
# Quarantine on all branches, linked to the issue tracking the fix.
mergify tests quarantines add -r owner/repo \
  --reason 'flaky: login times out under load' \
  --issue-url 'https://linear.app/acme/issue/ACME-1234' \
  'test_login'

# Scope the quarantine to one branch, JSON output.
mergify tests quarantines add -r owner/repo \
  --reason 'broken on release branch' --branch-name release/1.0 --json \
  'test_logout'

# Scope it to the one job where the test is flaky, on main.
mergify tests quarantines add -r owner/repo \
  --reason 'flaky on the macOS runner' \
  --branch-name main --pipeline-name ci --job-name 'tests (macos)' \
  --issue-url 'https://github.com/owner/repo/issues/42' \
  'test_upload'
```

A quarantine's scope is a branch, a pipeline and a job; each one omitted means
all of them. Scope it as narrowly as the failure: a test flaky in one job stays
blocking in the others, so real failures there still show. A test can hold one
quarantine per scope; then `remove` and `get` by name refuse to pick one and
list the ids: pass the id of the one to target.

**Key options:**
- `--repository` / `-r` -- Repository full name (`owner/repo`); auto-detected from the CI environment or the local git remote when omitted.
- `--reason` -- Reason recorded for the quarantine; required.
- `--branch-name` -- Branch to scope to. Omit for all branches.
- `--pipeline-name` -- Pipeline to scope to. Omit for all pipelines.
- `--job-name` -- Job to scope to. Omit for all jobs.
- `--issue-url` -- Link to the issue tracking the fix, in any issue tracker. Must be an `http` or `https` URL; the API refuses anything else.
- `--token` / `-t` (env: `MERGIFY_TOKEN`) -- Mergify credential. Falls back to the credential `mergify auth login` stored. The CLI no longer falls back to `GITHUB_TOKEN` or `gh auth token`.
- `--api-url` / `-u` (env: `MERGIFY_API_URL`) -- API base URL.
- `--json` -- Emit `{"id", "test_name", "reason", "branch_name", "issue_url", "pipeline_name", "job_name"}` to stdout.

**Exit codes:**
- `0` -- Test quarantined.
- `6` -- Mergify API error (e.g. the test is already quarantined).

## Tests Quarantines Remove (`tests quarantines remove`)

Removes a test from the quarantine. Accepts either the fully qualified test
name (resolved to its quarantine id via the list endpoint) or the quarantine
id directly (as printed by `tests quarantines add`). A UUID-shaped argument
is treated as the id and deleted without a lookup.

Needs an `admin` key or a user credential (`mergify auth login`) — a `ci` key gets a `403`. See
[Authentication](#authentication).

```bash
# By test name.
mergify tests quarantines remove -r owner/repo 'test_login'

# By quarantine id (the value `tests quarantines add` printed).
mergify tests quarantines remove -r owner/repo 12345678-1234-5678-1234-567812345678
```

**Key options:**
- `--repository` / `-r` -- Repository full name (`owner/repo`); auto-detected from the CI environment or the local git remote when omitted.
- `--token` / `-t` (env: `MERGIFY_TOKEN`) -- Mergify credential. Falls back to the credential `mergify auth login` stored. The CLI no longer falls back to `GITHUB_TOKEN` or `gh auth token`.
- `--api-url` / `-u` (env: `MERGIFY_API_URL`) -- API base URL.
- `--json` -- Emit `{"id", "test_name"}` to stdout (`test_name` is null when
  addressed by id).

**Exit codes:**
- `0` -- Test unquarantined.
- `6` -- Mergify API error (e.g. the test is not quarantined).
- `7` -- The test name matches several quarantines; pass an id.

## Tests Quarantines Get (`tests quarantines get`)

Prints a single quarantine, addressed by the fully qualified test name or the
quarantine id (a UUID-shaped argument is matched against the id). The output
mirrors one record of `tests quarantines list`.

```bash
# By test name.
mergify tests quarantines get -r owner/repo 'test_login'

# By quarantine id, JSON output.
mergify tests quarantines get -r owner/repo --json \
  12345678-1234-5678-1234-567812345678
```

**Key options:**
- `--repository` / `-r` -- Repository full name (`owner/repo`); auto-detected from the CI environment or the local git remote when omitted.
- `--token` / `-t` (env: `MERGIFY_TOKEN`) -- Mergify credential. Falls back to the credential `mergify auth login` stored. The CLI no longer falls back to `GITHUB_TOKEN` or `gh auth token`.
- `--api-url` / `-u` (env: `MERGIFY_API_URL`) -- API base URL.
- `--json` -- Emit the record (`id`, `test_name`, `reason`, `branch_name`, `issue_url`,
  `pipeline_name`, `job_name`, `created_at`, `source`, `is_recovered`) to stdout.

**Exit codes:**
- `0` -- Quarantine found and printed.
- `6` -- Mergify API error (e.g. no matching quarantine).
- `7` -- The test name matches several quarantines; pass an id.

## Tests Quarantines List (`tests quarantines list`)

Lists every test currently in the repository's Test Engine quarantine. Takes no
test argument -- it prints the whole quarantine. Human output is one indented
block per record -- the test name on its own line (never wrapped mid-name),
then its id (the value `delete` accepts), branch, pipeline, job, source,
recovered, issue (only when one is linked), and reason. `--json` emits the
full records.

```bash
# Human output (one block per record).
mergify tests quarantines list -r owner/repo

# JSON for jq -- e.g. names of quarantines an auto-recover run flagged.
mergify tests quarantines list -r owner/repo --json \
  | jq -r '.quarantined_tests[] | select(.is_recovered) | .test_name'
```

A null `branch_name`, `pipeline_name` or `job_name` renders as `*` (the quarantine
applies to all branches, pipelines or jobs).
`source` is `manual` (added by a user) or `auto` (added by flaky detection).
`is_recovered` flags quarantines whose recent runs suggest they can be removed.
A null `issue_url` means no issue tracks the fix.

**Key options:**
- `--repository` / `-r` -- Repository full name (`owner/repo`); auto-detected from the CI environment or the local git remote when omitted.
- `--token` / `-t` (env: `MERGIFY_TOKEN`) -- Mergify credential. Falls back to the credential `mergify auth login` stored. The CLI no longer falls back to `GITHUB_TOKEN` or `gh auth token`.
- `--api-url` / `-u` (env: `MERGIFY_API_URL`) -- API base URL.
- `--json` -- Emit `{"quarantined_tests": [...]}` to stdout, each record carrying
  `id`, `test_name`, `reason`, `branch_name`, `issue_url`, `pipeline_name`,
  `job_name`, `created_at`, `source`, `is_recovered`.

**Exit codes:**
- `0` -- Always, including an empty quarantine ("No quarantined tests found.").

## Queue Info (`queue-info`)

Outputs the current build's merge queue batch metadata as JSON. Reads the `refs/notes/mergify/<branch>` git note Mergify writes on the merge queue branch head the build is for; exits `INVALID_STATE` (7) when no note is found (i.e. not a merge queue batch build, or a pull request from a fork). Takes no arguments and needs no GitHub token.

It reads the note `ci git-refs` reads, fetching that branch's notes ref alone. On a pull request build it reads no other:

- On a GitHub Actions pull request event (`pull_request`, `pull_request_target`, a review), the note for the pull request's branch on its head (`pull_request.head`), so the default `actions/checkout` of a `pull_request` event, GitHub's merge of the draft into its base, is fine. A pull request from a fork gets none.
- On a Jenkins change request (`CHANGE_ID` set), the note for the branch the change request comes from (`CHANGE_BRANCH`), on the commit Jenkins checked out (`GIT_COMMIT`, else `HEAD`), or on the first parent of a merge, as when Jenkins merged a target that moved on since into the draft. A fork's change request (`CHANGE_FORK`) gets none.

On a branch build it reads the note for the branch built, on the commit built: the pushed branch on GitHub Actions `push`, `BUILDKITE_BRANCH` on `BUILDKITE_COMMIT` on Buildkite, `GIT_BRANCH` (else `BRANCH_NAME`) on `HEAD` on a Jenkins branch build. Without that note, it reads the note on `HEAD`, as on any other build.

The note's full payload is emitted verbatim (every field Mergify wrote, e.g. `checking_base_sha`, `pull_requests` with per-PR `scopes`, `previous_failed_batches`, top-level `scopes`). New fields appear automatically without a CLI update, so don't assume a fixed schema.

```bash
# In CI, on a merge queue batch build. Works in any CI (GitHub Actions,
# GitLab, CircleCI, Jenkins, ...) with plain git. When GITHUB_OUTPUT is
# set (GitHub Actions runner) it also writes the metadata there.
mergify ci queue-info
```

Mergify attaches the batch metadata as a git note to the MQ branch head commit, so reading it needs only git, no PR body and no GitHub token. To read the note on `HEAD` (GitLab, CircleCI, another GitHub Actions event, a branch build without its branch's note, ...), the command runs `git fetch origin "refs/notes/mergify/*"` itself (notes aren't fetched by default), then returns the note attached to `HEAD`. Requirements: the `origin` remote is reachable and, off a pull request build, the checkout is at the MQ branch head commit, which is the normal state inside a merge-queue CI run. A detached HEAD is fine.

This command is useful in CI workflows that need to know whether the current run is part of a merge queue batch and what other PRs are in the batch.

## Common Patterns

### Full CI pipeline with quarantine
```yaml
jobs:
  test:
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
      - name: Run tests
        run: pytest --junitxml=results.xml
      - name: Upload and evaluate
        if: always()
        run: mergify ci junit-process results.xml
        env:
          MERGIFY_TOKEN: ${{ secrets.MERGIFY_TOKEN }}
```

### Selective testing with scopes
```yaml
jobs:
  detect:
    outputs:
      scopes: ${{ steps.scopes.outputs.scopes }}
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          fetch-depth: 0
      - id: scopes
        run: mergify ci scopes --config .mergify.yml

  backend:
    needs: detect
    if: fromJSON(needs.detect.outputs.scopes).backend == 'true'
    steps:
      - run: pytest backend/
```

## When a command or flag is missing

If `mergify` rejects a command or flag this skill names, the CLI is probably older than the skill: follow the `mergify-cli-upgrade` skill.
