# Check that every job you changed sends its results

The goal: when the pull request merges, Mergify receives results from every job
you set up. Check it on the pull request's own CI run, at its head commit, job
by job, for exactly the jobs you changed.

## Wait for the jobs

A `gh` error, a commit with no run yet, or a filter that matches no job never
counts as "finished": the loop keeps waiting. It works in bash and zsh. Wait in the
foreground with this loop, never with a background process,
`run_in_background` or a monitor: the session ends with your answer, and a
background wait dies with it. Never promise to report later. The loop stops
after 9 minutes: run it with a 10-minute timeout, and again until every line
it prints ends in `done`. If your tool moved a wait to the background anyway, stop it first.
Put a plain regular expression matching the jobs you changed in `test(...)`,
with no quote or backslash (for example `^(test|build)`); when job names repeat
across workflows, add `--workflow <file>` to `gh run list`. Before trusting
`done`, check that every entry of your "Jobs changed" list is in the output: a
missing one means its run has not started yet, so run the loop again.

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

Then, for every run at the head commit (`gh run list --commit "$sha"`: with
`on: [push, pull_request]` there are two, and both count), list its jobs and
read the log of each job you changed:

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
- A run on a pull request from a branch of the same repository receives the
  repository's secrets, so the upload is real.
- If you cannot finish (rate limit, a job still going after about 20 minutes),
  report the remaining jobs as **not verified yet**, with the commands above
  for the user to run.

## Classify each job you changed

Put every job you changed (every matrix leg counts) in exactly one case. A leg
you left without the plugin on purpose (PyPy, free-threaded Python, a Ruby
below 3.1) is in none: list it as "cannot report" with its reason.

1. **Did not run on this commit.** No run for its workflow at the head commit,
   or the job is `skipped`. Find why in the workflow: `on:` without
   `pull_request`, `paths` or `branches` filters, `workflow_dispatch` or
   `schedule` only, an `if:` condition, a job the matrix excludes. Say so:
   it is not a setup failure, and it reports once it runs.
2. **Ran, success trace in its log.** Done.
3. **Ran, error trace.** Fix it, or tell the user what to do (table below).
4. **Ran, no Mergify trace at all.** First rule out what is not a setup
   problem: a JavaScript job on Node.js below 22, a JavaScript plugin up to
   2026.10.5 (silent on success), a tool that captures the test output. Else
   it is a setup problem: the plugin is not loaded, the token is on another
   step, the wrong job was changed, or the tests ran inside a container
   without the variables. Fix it, push, and check again.

## Traces

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
the upload works. On those JavaScript versions, a job with no error line is
"no error, upload not visible in the log", never confirmed.

The JUnit upload (`junit-process`, `gha-mergify-ci`) prints `Run ID:` and
`report uploaded` on success, `Failed to upload test results` on failure.

A JavaScript job on Node.js below 22 prints no Mergify line at all: that is the
Node.js version, not a setup mistake (`vitest.md`, `playwright.md`).

What to do on an error:

- **`HTTP 422` … `CI Insights is not enabled on this repository`** (in the
  `Error:` line): the user enables CI Insights for the repository on the
  Repositories page of the Mergify dashboard, then re-runs the job.
- **No token** (`MERGIFY_TOKEN is not set`; before 2026.10.6 `No token
  configured` or `WARNING: MERGIFY_TOKEN is not set`): the
  secret is not stored yet, or not passed to that step. Check the step's
  `env`; if it is there, the user stores `MERGIFY_TOKEN` and re-runs the job.
- **An error from the plugin itself** (for example `Failed to flush spans`):
  a plugin bug, not a setup mistake. Report it with the line, and do not call
  that job set up.

## Other CIs

On CircleCI, Jenkins and Buildkite, read the job logs with the CI's own CLI if
it is installed and authenticated. Otherwise do not wait for the run, and do
not poll GitHub for a CI that reports nothing there: give the user the traces
above to look for in each job's log, and end.
