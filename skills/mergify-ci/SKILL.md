---
name: mergify-ci
description: Use Mergify CI commands to upload JUnit test results, detect git references, manage CI scopes, retrieve merge queue metadata, and write the OIDC trust policies that let GitHub Actions jobs reach Mergify without a key. ALWAYS use this skill when running `mergify ci`, `mergify tests` or `mergify oidc` commands, uploading test results to Mergify, managing Mergify test quarantines, turning Mergify test retry on or off, detecting Mergify CI scopes, reading Mergify merge queue CI context, or finding out why a job's ID token was refused. Triggers on mergify ci, mergify tests, mergify oidc, junit-process, Mergify Test Engine, CI Insights, Mergify quarantine, Mergify test retry, Mergify scopes, scopes-send, git-refs, queue-info, trust policy, OIDC, id-token.
---

# Mergify CI Commands

## Overview

The `mergify ci` command group provides tools for CI pipelines: uploading JUnit test results to Mergify Test Engine, detecting git references for diff-based operations, managing CI scopes for selective testing, and retrieving merge queue batch metadata. `mergify oidc` writes the trust policies that let a GitHub Actions job reach Mergify with its ID token instead of a key, and reads why a job was refused.

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
mergify tests retry show                  # Say whether test retry is on, and with which settings
mergify tests retry on                    # Turn test retry on (or change its settings)
mergify tests retry set                   # Change the settings of test retry while it is on
mergify tests retry off                   # Turn test retry off
mergify oidc trust-policies list          # List the organization's OIDC trust policies
mergify oidc trust-policies create        # Write a trust policy (show, update, delete too)
mergify oidc exchanges list               # List the latest ID token exchanges, and why one was refused
mergify oidc exchanges show ID            # Show the exchange a refused job's log names
```

## Authentication

A Mergify application key, minted in the dashboard, holds the
[permissions](https://docs.mergify.com/api/application-keys/) it was granted,
resource by resource, each at the level it was granted. A CI key holds CI
Reporting and nothing else; a key granted every resource does not hold it.

| Key | Reaches |
| --- | --- |
| CI key | What a CI job does: trace upload, `scopes-send`, quarantine *evaluation* (`junit-process`), the quarantine *list* and a commit's test runs. |
| A key without CI Reporting | What its permissions name: Tests at Read for test health and test runs, Quarantines at Read for the quarantine *list*, at Read & write to change the quarantine, Test Retry Settings for test retry's settings. Never trace upload, never `scopes-send`. |

Trace upload and `scopes-send` need CI Reporting, so a CI key, or on GitHub
Actions the job's ID token ([below](#github-actions-jobs-need-no-key-oidc)): a
key without CI Reporting or a user credential gets a `403`. Every other endpoint below that refuses a CI
key accepts a user credential (`mergify auth login`, which your user runs: see
[below](#when-a-command-says-there-is-no-mergify-credential)) instead. `GITHUB_TOKEN`
inside GitHub Actions is not a Mergify credential at all, so do not reach for
it to clear one of these `403`s.

The split follows the endpoint each command calls: reads of test health and
writes to the quarantine are a person inspecting or overriding a repository,
not something a pipeline does, so they are outside what a CI key carries;
trace upload and scope reports are what a pipeline sends, so they are outside
what a key without CI Reporting carries. Per command:

| Command | CI key | A key without CI Reporting needs |
| --- | --- | --- |
| `ci junit-process`, `ci junit-upload` | yes | Quarantines at Read, for quarantine evaluation only: the upload gets a `403`, reported as a rejected upload (the exit code is unaffected) |
| `ci scopes-send` | yes | **refused**: `403` |
| `tests runs` | yes | Tests at Read (or `mergify auth login`) |
| `tests quarantines list`, `tests quarantines get` | yes — both read the quarantine list | Quarantines at Read |
| `tests show` | **no** — `403` | Tests at Read |
| `tests quarantines add`, `tests quarantines remove` | **no** — `403` | Quarantines at Read & write |
| `tests retry show`, `on`, `set`, `off` | **no** — `403` | Test Retry Settings at Read for `show`, Read & write for the others |

`ci git-refs`, `ci scopes` and `ci queue-info` are evaluated locally and need
no token at all.

### GitHub Actions jobs need no key (OIDC)

On GitHub Actions, a job granted `id-token: write` with no `MERGIFY_TOKEN`
trades its GitHub ID token for a short-lived Mergify OIDC CI token that does
what the trust policies matching the job grant, CI Reporting by default, on its
own repository only, when a trust policy of the organization matches the job: `ci junit-process`, `ci scopes-send` and the test
framework plugins (2026.11.2 or later) do it themselves. A `MERGIFY_TOKEN` that
is set always wins. Writing a trust policy needs an organization admin or CI
Admin: see [OIDC Trust Policies](#oidc-trust-policies-mergify-oidc).
Fork pull requests get no ID token, as they get no secret. Jobs outside
GitHub Actions keep a CI key.

## JUnit Processing (`junit-process`)

The primary CI command. Parses JUnit XML reports, checks quarantine status for failing tests, uploads results to Mergify Test Engine, and determines the final CI exit code.

```bash
mergify ci junit-process \
  --repository owner/repo \
  --tests-target-branch main \
  path/to/junit-results.xml
```

On GitHub Actions, a job granted `id-token: write` needs no key (see [GitHub Actions jobs need no key](#github-actions-jobs-need-no-key-oidc)). On any other CI, the key is read from `MERGIFY_TOKEN`: set it in the job's environment from a secret rather than passing `--token`, which would put it in the process list.

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
8. With no `MERGIFY_TOKEN`, a job whose ID token Mergify refuses (no trust policy matches it yet) or does not exchange carries on without Mergify: nothing is uploaded and no test is quarantined, so it exits as points 5 and 6 say with every failure blocking. The report says `MERGIFY_TOKEN is not set, skipping the test results upload and the quarantine check.`, followed by Mergify's reason when it refused, and so does a `::warning::` annotation on GitHub Actions; `$GITHUB_OUTPUT` gets `test_results_upload=rejected`. A Mergify that cannot answer the exchange is an outage instead, reported as in point 7

**GitHub Actions example:**
```yaml
jobs:
  test:
    permissions:
      id-token: write
      contents: read
    steps:
      # ...
      - name: Run tests
        id: tests
        run: pytest --junitxml=results.xml || echo "exit_code=$?" >> "$GITHUB_OUTPUT"

      - name: Process test results
        if: always() && (!github.event.pull_request || github.event.pull_request.head.repo.full_name == github.repository)
        run: |
          mergify ci junit-process \
            --test-exit-code ${{ steps.tests.outputs.exit_code || 0 }} \
            results.xml
```

## Git References (`git-refs`)

Detects the base and head git references for the current pull request context. Writes results to `GITHUB_OUTPUT` (`base`, `head`, `source`) when running in GitHub Actions, and to Buildkite meta-data (`mergify-ci.base`, `mergify-ci.head`, `mergify-ci.source`) when running in Buildkite.

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
# `eval "$(...)"` would go unnoticed). --for-scopes: nx picks the
# projects to build, the scopes of the build.
refs=$(mergify ci git-refs --format=shell --for-scopes) || exit 1
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

A merge queue batch built on top of other batches has two bases. The base `git-refs` gives is the queue base: the commit of the base branch below every batch the queue branch is built on, which the note carries as `queue_base_sha`. A batch that passes can merge the batches it is built on too (`skip_intermediate_results`), so a diff from the queue base holds every pull request its pass merges, which is what a check no scope selects has to read (a secret scan, a dependency audit, a schema compatibility check). With `--for-scopes`, the base is the top of those batches, the note's `checking_base_sha`, so a diff from it holds the batch's own pull requests: what the batch's scopes are detected from, as `ci scopes` does. The two are the same on a batch built on nothing else, and on any other build. On a note without `queue_base_sha`, both are `checking_base_sha`.

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

The config file defines scopes with file patterns. When files change between base and head, matching scopes are identified and written to `GITHUB_OUTPUT` (on GitHub Actions) or to Buildkite meta-data under `mergify-ci.scopes` (on Buildkite), as a JSON map of scope names to `"true"`/`"false"`. On GitHub Actions it also writes the `base`, `head` and `source` outputs `git-refs` writes, so one step gives both. It compares from the base `git-refs --for-scopes` gives, and on a merge queue batch whose note lists its scopes it takes those instead.

When the base is the `before` commit detected from a GitHub `push` event, the changed files are the ones that differ between it and the pushed commit, compared directly rather than from their merge base: a push back to an earlier commit selects the scopes of the files it reverts. Every other base, an explicit `--base` included, counts what `head` adds on top of it, from their merge base.

On a merge queue batch build (a draft pull request, or a `push` build of the merge queue branch), scopes are not computed from changed files: the command reads the engine's git note for the batch and returns the scopes the merge queue decided (the batch's scopes plus each batched pull request's scopes, including scopes a pull request's CI reported), plus the merge queue scope. A batch the note marks as a barrier (`all_scopes: true`) selects every scope. This also works with a `manual` scopes source. Without a note, or with a note that has no `scopes` list, it falls back to diffing files.

A renamed file counts against **both** of its paths, same as the engine: `git mv critical/guard.txt ignored/guard.txt` touches `critical` and `ignored`.

With a source that declares `manual` beside `files`, the command detects the `files` part and says on stderr that Mergify adds the scopes sent with `scopes-send` to them: see [Adding scopes to the file patterns](#adding-scopes-to-the-file-patterns). With `manual` alone it fails outside a merge queue batch build, as there are no patterns to match.

## Scopes Send (`scopes-send`)

Sends scopes tied to a pull request to the Mergify API. The merge queue takes them from a scopes source that declares `manual`: alone, they are the pull request's only scopes; beside `files`, they are added to those the file patterns match.

The report is addressed by the pull request's **head SHA**, so it says which revision it was computed for and a result computed for an older head cannot be taken for the current one. The head is detected from the CI environment (GitHub Actions event payload, `BUILDKITE_COMMIT`, `CIRCLE_SHA1`, or a Jenkins pull request build's checkout); pass `--head-sha` to name it explicitly wherever it is not detected: a GitHub Actions event with no `pull_request` payload (`push`, `workflow_run`, ...), any other CI. A Jenkins multibranch pull request build needs no flag: the number comes from `CHANGE_ID`, and the head is the commit checked out (`GIT_COMMIT`, else `HEAD`) or, when the GitHub Branch Source plugin merges the pull request into its target branch, the first parent of the merge Jenkins made. No head is detected where git cannot read the checkout, such as a container without its `.git`, nor on an agent that merges with JGit, whose merge Jenkins does not make under its own name: pass `--head-sha` with the pull request's head there, or switch the source's strategy to "The current pull request revision". When no head SHA can be resolved, the command reports against the pull request number alone and warns: that route is deprecated and will be removed. A Mergify deployment that predates the commit endpoint (on-premise before 8.18.0) gets the same pull request route without a warning.

Needs a CI key, or on GitHub Actions the job's ID token; a key without CI
Reporting or a user credential gets a `403`. See [Authentication](#authentication).

When Mergify fails, times out or throttles the upload (a `5xx`, `408` or `429`
once the retries run out) or does not answer, the command fails. With
`--skip-when-mergify-unavailable` it warns and exits 0 instead, so a required
check that runs it stays green while Mergify is down: the merge queue waits for
the scopes until the job runs again. Any other `4xx` fails it either way.

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

### Adding scopes to the file patterns

To add scopes on top of the file patterns, declare `manual: null` beside `files` (`barrier_files` still applies):

```yaml
scopes:
  source:
    files:
      frontend:
        include:
          - apps/web/**/*
    manual: null
```

A pull request's scopes are then those its changed files match plus those reported for its head commit. It is a barrier when it changes a `barrier_files` file or the report carries `--all`. The merge queue waits for that report, so send one for every pull request, even with nothing to add. Only a report addressed by the head SHA counts. With `files` alone, the merge queue reads the file patterns and `barrier_files`, not what `scopes-send` reports.

Send extra scopes for what the patterns cannot see, such as the projects a build tool finds affected through dependencies, or a decision a CI step makes. Send `--all` for a change that impacts every scope and that `barrier_files` does not catch.

Send the detection and the extras in **one** report, since a second report for the same head commit replaces the first:

```bash
mergify ci scopes --write scopes.json
mergify ci scopes-send --scopes-json scopes.json -s api-contract -s e2e
```

## Tests Show (`tests show`)

Looks up tests by name on the repository's default branch and prints their
health, success/failure ratios, and last failure context. The search is a
batch API: pass one or more names (globs supported) and one block per match
is rendered. It is read-only: it exits `0` once it has rendered the matches,
whatever their health. Gate on health by consuming `--json`, not the exit code.

A last run whose first attempt failed and that then passed on a retry reads
`✓ passed on a retry (its first attempt failed)`. In `--json`, each test's
`last_run_passed_on_retry` is then `true`, and its `last_conclusion` stays
`failed`, as it reports the first attempt. An engine that does not report
retries yet leaves `last_run_passed_on_retry` out: absent means unknown, not
`false`.

Needs an application key with Tests at Read or a user credential (`mergify auth login`): a CI key gets a `403`. See
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
- `6` -- Mergify API error, including the `403` a CI key gets here.

## Tests Runs (`tests runs`)

A test result upload that fails leaves the job green: the plugin exits 0
when Mergify refuses the upload, and results sent under the wrong job name
pass unnoticed. `mergify tests runs` lists the test runs Mergify received
for a commit (the `Test run ID: …` a plugin prints in the CI log), one line
per job name they arrived under (`MERGIFY_TEST_JOB_NAME`, else the CI's job
name — on GitHub Actions the job id, not its displayed name): how many runs,
how many arrived whole, framework, plugin or converted report and the
client's version, test counts, and how many of the failures then passed on a
retry. Only a run that did not arrive whole gets a line of its own. In
`--json`, that count is `passed_on_retry` on each run under
`jobs[].test_runs[]`, each such test also counted in the run's `failed`; an
engine that does not report retries yet leaves it out: absent means unknown,
not zero.

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

Needs an application key with Quarantines at Read & write or a user credential (`mergify auth login`): a CI key gets a `403`. See
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

# Let Mergify unquarantine it once it's healthy again, after the fix lands.
mergify tests quarantines add -r owner/repo \
  --reason 'flaky, fix in progress' --unquarantine-when-healthy \
  'test_checkout'
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
- `--unquarantine-when-healthy` -- Mergify removes the quarantine once the test is healthy again, when it no longer detects it as flaky or broken. Off by default: the quarantine stays until it is removed. It cannot be changed on an existing quarantine: remove it and add it again.
- `--token` / `-t` (env: `MERGIFY_TOKEN`) -- Mergify credential. Falls back to the credential `mergify auth login` stored. The CLI no longer falls back to `GITHUB_TOKEN` or `gh auth token`.
- `--api-url` / `-u` (env: `MERGIFY_API_URL`) -- API base URL.
- `--json` -- Emit `{"id", "test_name", "reason", "branch_name", "issue_url", "pipeline_name", "job_name", "unquarantine_when_healthy"}` to stdout.

**Exit codes:**
- `0` -- Test quarantined.
- `6` -- Mergify API error (e.g. the test is already quarantined).

## Tests Quarantines Remove (`tests quarantines remove`)

Removes a test from the quarantine. Accepts either the fully qualified test
name (resolved to its quarantine id via the list endpoint) or the quarantine
id directly (as printed by `tests quarantines add`). A UUID-shaped argument
is treated as the id and deleted without a lookup.

Needs an application key with Quarantines at Read & write or a user credential (`mergify auth login`): a CI key gets a `403`. See
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
  `pipeline_name`, `job_name`, `created_at`, `source`, `unquarantine_when_healthy`,
  `is_recovered`) to stdout.

**Exit codes:**
- `0` -- Quarantine found and printed.
- `6` -- Mergify API error (e.g. no matching quarantine).
- `7` -- The test name matches several quarantines; pass an id.

## Tests Quarantines List (`tests quarantines list`)

Lists every test currently in the repository's Test Engine quarantine. Takes no
test argument -- it prints the whole quarantine. Human output is one indented
block per record -- the test name on its own line (never wrapped mid-name),
then its id (the value `remove` accepts), branch, pipeline, job, source,
recovered, unquarantine, issue (only when one is linked), and reason. `--json` emits the
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
`unquarantine_when_healthy` (`unquarantine: when healthy` in the human output,
`manually` otherwise) is true when Mergify removes the quarantine itself once
the test is healthy again; an auto-quarantine takes it from the repository's
auto-quarantine settings.
A null `issue_url` means no issue tracks the fix.

**Key options:**
- `--repository` / `-r` -- Repository full name (`owner/repo`); auto-detected from the CI environment or the local git remote when omitted.
- `--token` / `-t` (env: `MERGIFY_TOKEN`) -- Mergify credential. Falls back to the credential `mergify auth login` stored. The CLI no longer falls back to `GITHUB_TOKEN` or `gh auth token`.
- `--api-url` / `-u` (env: `MERGIFY_API_URL`) -- API base URL.
- `--json` -- Emit `{"quarantined_tests": [...]}` to stdout, each record carrying
  `id`, `test_name`, `reason`, `branch_name`, `issue_url`, `pipeline_name`,
  `job_name`, `created_at`, `source`, `unquarantine_when_healthy`, `is_recovered`.

**Exit codes:**
- `0` -- Always, including an empty quarantine ("No quarantined tests found.").

## Tests Retry (`tests retry`)

With test retry, the Mergify plugin runs a failing test again in the same job,
so a flaky failure no longer fails the build. It runs on pushes to the default
branch and in the runs the merge queue checks, in jobs that run a Mergify test
framework plugin: a JUnit report upload never retries. It is set per
repository, and is available on plans that include quarantine. A job whose
environment sets `MERGIFY_TEST_RETRY_ENABLE=false` never retries.

Two settings limit it: the **Retry Budget**, the extra time a job may spend on
retries as a percentage of its tests' usual duration, and the **Retries per
Test**, how many times one failing test may run again.

Reading it needs read access to the repository, and changing it write access,
with a user credential (`mergify auth login`); or an application key with
Test Retry Settings at Read to read it, at Read & write to change it. A CI key
gets a `403`. See [Authentication](#authentication).

```bash
# On or off, with the settings and who changed them last.
mergify tests retry show -r owner/repo

# Turn it on: settings left out keep Mergify's defaults.
mergify tests retry on -r owner/repo
mergify tests retry on -r owner/repo --retry-budget 10% --retries-per-test 3

# Change a setting while it is on, or put Mergify's default back.
mergify tests retry set -r owner/repo --retry-budget default

# Turn it off: its settings are forgotten.
mergify tests retry off -r owner/repo
```

`set` never turns test retry on: on a repository with it off, it fails and
says `mergify tests retry on` does. `off` on a repository with it off
succeeds and says it was already off. Mergify refuses a value out of range and
says what it takes, in its own terms: the budget is a ratio there
(`budget_ratio`, 5% is `0.05`).

**Key options:**
- `--repository` / `-r` -- Repository full name (`owner/repo`); auto-detected from the CI environment or the local git remote when omitted.
- `--token` / `-t` (env: `MERGIFY_TOKEN`) -- Mergify credential. Falls back to the credential `mergify auth login` stored.
- `--api-url` / `-u` (env: `MERGIFY_API_URL`) -- API base URL.
- `--retry-budget PERCENT` (`on`, `set`) -- The Retry Budget, written with its `%`, such as `5%`: a bare number is refused, as `--json` gives the budget as a ratio (`0.05`). `set` also takes `default`.
- `--retries-per-test N` (`on`, `set`) -- The Retries per Test; `set` also takes `default`.
- `--json` -- Emit `{"enabled": true, "configuration": {...}}` with the
  configuration as Mergify answered it (`budget_ratio`, the budget as a
  ratio, and `max_retries_per_test`, each `{value, chosen, default}`, then `created_at`,
  `created_by`, `updated_at`, `updated_by`), or `{"enabled": false,
  "defaults": {"budget_ratio": 0.05, "max_retries_per_test": 5}}` when it is
  off, `off` included.

**Exit codes:**
- `0` -- Done, or read; off is an answer, not an error.
- `2` -- An option the CLI cannot read, such as a budget without its `%`.
- `6` -- Mergify API error: no access, a plan without test retry, a Mergify that does not offer it, or an answer this CLI cannot read (after `on`, `set` or `off`, the change was made all the same: `--json` prints the answer).
- `7` -- `set` on a repository with test retry off.
- `8` -- A configuration problem: Mergify refused a setting's value, no token was found, or no repository was given or detected.

## OIDC Trust Policies (`mergify oidc`)

A GitHub Actions job gets a Mergify OIDC CI token for its ID token when one
trust policy of its organization matches it, and a policy matches when each of
its lines does: repositories, workflows, events and environments. Policies only
allow, with no order and no deny, so a narrow one cannot hold back a broad one.
The token may do on the job's repository what the policies matching it grant,
together: CI Reporting unless one grants more (below).

Every `mergify oidc` command takes the organization with `--org`, else the
owner of the current repository, and the credential of one of its admins or CI
Admins (`mergify auth login`), or an application key with the OIDC trust
policies permission, and the repositories one to name repositories (a
key granted every resource at Read & write has both); a CI key gets a `403`. Every command takes `--json`,
which prints what Mergify answered; `exchanges list --json` puts the exchanges
under `exchanges`, beside `since` and `truncated` (true when `--limit` left
some out).

```bash
# Every policy, line by line.
mergify oidc trust-policies list --org acme

# The narrowest useful policy: one repository's CI jobs, on the CI events
# (push, pull_request, merge_group, schedule, workflow_dispatch), any
# workflow, any environment.
mergify oidc trust-policies create --org acme --name "api CI" --repository api

# Every private and internal repository, new ones included, never a public
# one.
mergify oidc trust-policies create --org acme --name "private CI" \
  --repository-visibility private --repository-visibility internal

# Deploys only: one workflow, on push, in the production environment.
mergify oidc trust-policies create --org acme --name "api deploys" \
  --repository api --workflow .github/workflows/deploy.yml \
  --event push --environment production

# Change only the lines named; the others keep their values. A policy's ID
# is the UUID `list` shows before its name.
mergify oidc trust-policies update 0b6f3c2e-5f1d-4c7a-9e2b-8d4a1f0c3e57 --org acme \
  --repository api --repository web

mergify oidc trust-policies delete 0b6f3c2e-5f1d-4c7a-9e2b-8d4a1f0c3e57 --org acme
```

A line no flag names is the default on `create`: every repository, any
workflow, the CI events, any environment. `--name` alone therefore trusts every
CI job of the organization: ask the user before writing a policy, it decides
which jobs get a token.

**Lines:**
- `--repository NAME` (repeatable) -- only these repositories; `--except-repository NAME` -- every repository but these; `--all-repositories`. `NAME` or `OWNER/NAME`, of the organization's repositories Mergify's GitHub App can access. Or `--repository-visibility VISIBILITY` (repeatable: `public`, `internal`, `private`) -- every repository of these visibilities, which `list` shows as `every internal or private repository`.
- `--workflow PATH` (repeatable) -- the workflow file of the job's own repository its run started from, `.github/workflows/ci.yml`, at any ref; `--reusable-workflow OWNER/REPOSITORY/PATH[@REF]` -- a reusable workflow the job runs, even one of its own repository (`uses: ./.github/workflows/...`), optionally pinned to a full ref (`refs/heads/main`) or a commit sha; `--any-workflow`.
- `--event EVENT` (repeatable) -- replaces the CI events; `--any-event`. `pull_request_target`, `workflow_run` and `issue_comment` run with the repository's identity for people who cannot push to it: add them only when the user asks.
- `--environment NAME` (repeatable) -- a job in no environment does not match; `--any-environment`.

**More than CI Reporting.** A policy grants CI Reporting, which is all
`mergify ci` commands and the test framework plugins use. Every other `mergify`
command that calls Mergify's API trades the job's ID token for a token too, when
it has neither `--token`, `MERGIFY_TOKEN` nor a stored login, and that token
holds what the policies allowing the job grant: `mergify queue pause` in a job needs
`merge_queue_pause=write`, and a request it lacks is refused with a `403`. It
reaches the job's own repository only, so `mergify oidc` still needs a key. So
grant more only when the user asks for it, for a step that runs such a command
or calls Mergify's API with the job's OIDC CI token itself.
`--permission RESOURCE=LEVEL` (repeatable) adds an application key permission on
the job's own repository: `merge_queue_pause=write`, `quarantines=read`, or
`*=read` for every resource but CI Reporting. `--no-ci-reporting` leaves CI
Reporting out. On `update`, they replace what the policy grants;
`--permission ci_reporting=write` alone brings it back to CI Reporting. Only an
organization admin signed in as themselves writes, changes or deletes a policy
granting more: a CI Admin or an application key gets a `403` saying so. Anyone
who can push a branch to a repository the policy matches can edit a workflow to
get its grants, unless the policy names protected environments (`--environment`)
or reusable workflows pinned to a protected ref (`--reusable-workflow
OWNER/REPOSITORY/PATH@refs/heads/main`): write it that way.

**Visibility or a list.** Pick `--repository-visibility` when the rule is the
visibility itself, such as every private and internal repository and never a
public one: a repository created later gets tokens without editing the policy,
once Mergify's GitHub App can access it, and one made public stops getting them
at once. Pick `--repository` when only
some repositories' jobs should get a token, which is the narrowest policy, and
`--except-repository` to leave a few out. Mergify reads the visibility in the
job's ID token (`repository_visibility`): on a GitHub Enterprise Server whose
tokens do not give it, a policy by visibility matches no job, so name the
repositories there.

**Why a job was refused.** The job's log prints Mergify's reason, which names
the exchange: `The exchange report lists it as exchange
5e0c9a7d-2b14-4f3e-8a61-c9d07b2e4f83`. Read it:

```bash
mergify oidc exchanges show 5e0c9a7d-2b14-4f3e-8a61-c9d07b2e4f83 --org acme
mergify oidc exchanges list --org acme --result refused --repository api --since 1d
```

A refused exchange lists, per policy, the first line the job did not match
(`policy  <NAME> (<ID>) events: pull_request_target is not one of its events`): widen
that policy, or write another, with the user's go. `exchanges list` reads the last
7 days by default (`--since`, up to the 90 days the report keeps), latest
first, 20 at most (`--limit`). The refusal reasons:

| Reason | What to do |
| --- | --- |
| `no_policy` | The organization has no trust policy: write one. |
| `no_matching_policy` | No policy matches the job: see each policy's line above. |
| `repository_not_installed` | Mergify's GitHub App cannot access the repository: the user adds it to the app's installation. `--repository` cannot name such a repository: list without it. |
| `audience_mismatch` | The job asked for a token for another Mergify: check `MERGIFY_API_URL`. |

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
    permissions:
      id-token: write
      contents: read
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
      - name: Run tests
        run: pytest --junitxml=results.xml
      - name: Upload and evaluate
        if: always() && (!github.event.pull_request || github.event.pull_request.head.repo.full_name == github.repository)
        run: mergify ci junit-process results.xml
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

## When a command says there is no Mergify credential

On your user's machine, `no Mergify credential found` from `tests show`, `tests runs`, `tests quarantines` or `oidc` means neither `--token`, `MERGIFY_TOKEN` nor a stored login gave the CLI a credential. Stop and ask your user to log in:

- Don't run `mergify auth login` yourself. It prints a URL and a code, then waits until a person approves it in a browser: you cannot finish it, and the tool call hangs.
- In Claude Code, ask them to type `! mergify auth login` at the prompt. Elsewhere, ask them to run `mergify auth login` in their own terminal.
- Wait until they say it's done, then run the same command again.
- Don't read another source instead (CI logs, the GitHub API) without asking them first.

`ci junit-process` and `ci scopes-send` say something else when they find no credential: they need an application key in `MERGIFY_TOKEN` (or OIDC on GitHub Actions), which logging in does not provide.

## When a command or flag is missing

If `mergify` rejects a command or flag this skill names, the CLI is probably older than the skill: follow the `mergify-cli-upgrade` skill.
