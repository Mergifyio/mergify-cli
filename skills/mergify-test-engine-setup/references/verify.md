# Check that every job you changed sends its results

The goal: when the pull request merges, Mergify receives results from every job
you set up. Check it on the pull request's own CI run, at its head commit, job
by job, for exactly the jobs you changed.

## Ask Mergify what it received

`mergify tests runs` lists the test runs Mergify received for a commit, one
entry per job name they arrived under, on every CI provider, without reading
any CI log. It is the check to use whenever the `mergify` CLI is installed
(2026.10.8 or later) and can authenticate: `MERGIFY_TOKEN` (any application
key), the credential `mergify auth login` stored, or, deprecated but still
accepted, the GitHub CLI's token. If the command is missing or refuses to
authenticate, do not install or log in for the user: read the CI logs instead
(below), and give the user the command to run themselves.

```bash
mergify tests runs --sha "$(git rev-parse HEAD)" --json   # HEAD = the commit you pushed
```

What to read in the JSON:

- `test_results_ingestion`: `not_enabled` means CI Insights is off on the
  repository and Mergify refuses every upload: the user enables it on the
  Repositories page of the Mergify dashboard. `paused` means Mergify is not
  ingesting for the organization right now: say so and stop.
- `pending_uploads` above 0: uploads still waiting to be ingested. Ask again
  in a minute.
- `jobs[]`: one entry per job name. A job reports under
  `MERGIFY_TEST_JOB_NAME` if you set it, else under the job's identifier in
  the CI (on GitHub Actions the job id, not its displayed name), with
  `pipeline_name` the workflow or pipeline. Each matrix leg reports under the
  `MERGIFY_TEST_JOB_NAME` you gave it (SKILL.md, step 5): look for that name.
  Several `test_runs` under one name are re-runs or shards, never proof that
  several legs uploaded: a leg with no name of its own cannot be verified here,
  so say so. Its `test_results` is
  `results_received` (every run arrived whole), `upload_error` (a run arrived
  incomplete or unreadable) or `ingesting` (ask again shortly). Each of its
  `test_runs` gives the framework, the plugin or report and its version, and
  the test counts.

Ask until every entry of your "Jobs changed" list is there with
`results_received` and `pending_uploads` is 0, or until its CI job has
finished without appearing (the loop below tells you, on GitHub Actions) and
`pending_uploads` is 0: an upload can still be waiting for ingestion after its
job ends. A job missing once its CI job has finished and nothing is pending
sent nothing Mergify received: read its log to see why. A name you did not expect, or two jobs under one name,
means the job name sent is not the one you meant: set `MERGIFY_TEST_JOB_NAME`.

## Wait for the jobs (GitHub Actions)

A `gh` error, a commit with no run yet, or a filter that matches no job never
counts as "finished": the loop keeps waiting. It works in bash and zsh. Wait in
the foreground with this loop, never with a background process,
`run_in_background` or a monitor: the session ends with your answer, and a
background wait dies with it. Never promise to report later. The loop stops
after 9 minutes: run it with a 10-minute timeout, and again until every line
it prints ends in `done`. If your tool moved a wait to the background anyway,
stop it first. Put a plain regular expression matching the jobs you changed in
`test(...)`, with no quote or backslash (for example `^(test|build)`); when job
names repeat across workflows, add `--workflow <file>` to `gh run list`. Before
trusting `done`, check that every entry of your "Jobs changed" list is in the
output: a missing one means its run has not started yet, so run the loop again.

```bash
sha=$(gh pr view <number> --json headRefOid --jq .headRefOid)
end=$((SECONDS + 540))
while [ "$SECONDS" -lt "$end" ]; do
  runs=$(gh run list --commit "$sha" --json databaseId --jq '.[].databaseId') || runs=""
  jobs=$(printf '%s\n' "$runs" | while IFS= read -r run; do
    [ -n "$run" ] || continue
    gh run view "$run" --json jobs --jq '.jobs[] | select(.name | test("<jobs you changed>"))
      | [.name, (if .conclusion == "" and ((.steps | length) == 0 or any(.steps[]; .status != "completed")) then "running" else "done" end)] | @tsv' \
      || printf 'gh failed on run %s\trunning\n' "$run"
  done)
  if [ -n "$jobs" ] && ! printf '%s\n' "$jobs" | grep -q 'running$'; then break; fi
  sleep 60
done
printf '%s\n' "${jobs:-no matching job yet}"
```

On another CI, with no CLI to read its runs, wait on Mergify instead, with
the same rules (foreground, run again until it stops early, at most 20 minutes
in all). Put in `want` the job names your "Jobs changed" list expects: the
loop stops only when every one of them has `results_received`. A job left in
`upload_error` or missing at the end is classified below, never counted as
received:

```bash
end=$((SECONDS + 540))
while [ "$SECONDS" -lt "$end" ]; do
  mergify tests runs --sha "$(git rev-parse HEAD)" --json > /tmp/mergify-runs.json 2>/dev/null \
    && jq -e --argjson want '["<job name>", "<job name>"]' \
         '.pending_uploads == 0 and (($want - [.jobs[] | select(.test_results == "results_received") | .job_name]) | length == 0)' \
         /tmp/mergify-runs.json >/dev/null \
    && break
  sleep 60
done
jq -r '.jobs[] | [.pipeline_name, .job_name, .test_results] | @tsv' /tmp/mergify-runs.json
```

- A run on a pull request from a branch of the same repository receives the
  repository's secrets, so the upload is real.
- If you cannot finish (rate limit, a job still going after about 20 minutes),
  report the remaining jobs as **not verified yet**, with the commands above
  for the user to run.

## Classify each job you changed

Put every job you changed (every matrix leg counts) in exactly one case. A leg
you left without the plugin on purpose (PyPy, free-threaded Python, a Ruby
below 3.1) is in none: list it as "cannot report" with its reason. A leg you
moved to the JUnit upload (`junit.md`) is classified like any other job.

1. **Did not run on this commit.** No run for its workflow at the head commit,
   or the job is `skipped`. Find why in the workflow: `on:` without
   `pull_request`, `paths` or `branches` filters, `workflow_dispatch` or
   `schedule` only, an `if:` condition, a job the matrix excludes. Say so:
   it is not a setup failure, and it reports once it runs.
2. **Results received.** `mergify tests runs` lists it with
   `results_received`, or, without the CLI, its log has a success trace.
3. **Upload failed.** `upload_error`, or a job that ran and is missing from
   `mergify tests runs` while its log has an error trace (table below): fix it,
   or tell the user what to do.
4. **Nothing sent.** It ran, it is missing from `mergify tests runs`, and its
   log has no Mergify line at all. First rule out what is not a setup problem:
   a plugin on a runtime it does not support (a JavaScript plugin on Node.js
   below 22, a Ruby the Mergify core does not cover), a tool that captures the
   test output. A JUnit upload works on any Node.js: never excuse it that way.
   Else it is a setup problem: the plugin is not loaded, the token is on
   another step, the wrong job was changed, or the tests ran inside a
   container without the variables. Fix it, push, and check again.

## Read a job's log

To see why a job is missing, or to verify without the CLI. Read every run at
the head commit (`gh run list --commit "$sha"`: with `on: [push,
pull_request]` there are two, and both count):

```bash
for run in $(gh run list --commit "$sha" --json databaseId --jq '.[].databaseId'); do
  gh run view "$run" --json jobs --jq '.jobs[]|[.databaseId,.name,.conclusion]|@tsv'
done
gh api --allow-escape-sequences repos/<owner>/<repository>/actions/jobs/<job id>/logs \
  | grep -n -E "Test run ID|Run ID:|report uploaded|upload this run|Failed to upload|is deprecated; set|No token configured|MERGIFY_TOKEN is not set|MERGIFY_TEST_RUN_ID"
```

- The jobs API returns a finished job's log even while GitHub still shows the
  job or the run `in_progress` (older `gh`: drop `--allow-escape-sequences`).
- An empty log means you could not read it, never "no Mergify trace".
- On CircleCI, Jenkins and Buildkite, read the logs with the CI's own CLI if
  it is installed and authenticated; otherwise give the user the traces below
  to look for.

From 2026.10.6, every plugin ends its run with a `Mergify CI` section (RSpec:
`Mergify CI:`) holding the same lines:

| Line | Meaning |
|---|---|
| `Test run ID: … (use it when contacting Mergify support)`, and no "couldn't upload" line | **success** |
| `Mergify couldn't upload this run's test results, so Test Engine won't show this run.` then `Error: …` | **error**: the upload failed (the run ID can still print) |
| `Mergify won't upload this run's test results: MERGIFY_TOKEN is not set.` | **error**: no token (missing or empty) |
| `Mergify won't upload this run's test results: the repository is unknown.` | **error**: the CI variables did not reach the tests (`containers.md`) |
| `… is deprecated; set … instead.` | a warning: rename the variable |

Releases up to 2026.10.5 print other lines: pytest `Test run ID:` in its
`Mergify CI` section, or `No token configured for Mergify`; RSpec
`--- Mergify CI ---` with `MERGIFY_TEST_RUN_ID=`, or
`WARNING: MERGIFY_TOKEN is not set`; Vitest and Playwright nothing at all when
the upload works, so without `mergify tests runs` such a job is "no error,
upload not visible in the log", never confirmed.

The JUnit upload (`junit-process`, `gha-mergify-ci`) prints `Run ID:` and
`report uploaded` on success, `Failed to upload test results` on failure.

A JavaScript plugin (Jest, Mocha, Vitest, Playwright) on Node.js below 22
prints `Mergify needs Node.js 22 or later (this is …), so nothing will be
sent.` (older plugin releases print no Mergify line at all): that is the
Node.js version, not a setup mistake. It does not apply to a JUnit upload,
whose job must show `Run ID:` and `report uploaded` whatever its Node.js. A
Ruby job the precompiled Mergify core does not cover (`minitest.md`) sends
nothing either.

What to do on an error:

- **`HTTP 422` … `CI Insights is not enabled on this repository`** (in the
  `Error:` line, or `test_results_ingestion: not_enabled`): the user enables CI
  Insights for the repository on the Repositories page of the Mergify
  dashboard, then re-runs the job.
- **No token** (`MERGIFY_TOKEN is not set`; before 2026.10.6 `No token
  configured` or `WARNING: MERGIFY_TOKEN is not set`): the secret is not
  stored yet, or not passed to that step. Check the step's `env`; if it is
  there, the user stores `MERGIFY_TOKEN` and re-runs the job.
- **An error from the plugin itself** (for example `Failed to flush spans`):
  a plugin bug, not a setup mistake. Report it with the line, and do not call
  that job set up.
