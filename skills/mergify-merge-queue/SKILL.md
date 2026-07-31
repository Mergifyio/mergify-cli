---
name: mergify-merge-queue
description: Use Mergify merge queue to queue/dequeue PRs, to monitor and inspect the queue, and to diagnose a dequeued PR — whether it is queued, why it was dequeued, where its CI failure is, and what to do next. ALWAYS use this skill when queuing or dequeuing a PR, checking queue status, investigating PR merge state, finding out why a PR left the queue, pausing/unpausing the queue, or debugging merge failures. Triggers on queue a PR, requeue, dequeue, dequeued, why was my PR dequeued, dequeue reason, merge queue, queue status, queue pause, queue show, pause, unpause, frozen, bisecting, batch, CI checks, CHECKS_FAILED, PULL_REQUEST_UPDATED.
---

# Mergify Merge Queue

## Overview

The merge queue serializes PR merges, running CI on temporary merge commits to catch integration failures before they reach the target branch. Use comments on the PR to queue/dequeue it, and the CLI to monitor queue state, inspect individual PRs, and manage the queue.

A PR that left the queue is diagnosed from **GitHub artifacts and the Mergify API**, not from the CLI — see [Diagnosing a dequeued PR](#diagnosing-a-dequeued-pr). Know that boundary before you start: `mergify queue show` only reports on PRs that are *currently* in the queue.

## Queuing and Dequeuing a PR

Queue, dequeue, and requeue actions are driven by **comments on the pull request**, not the CLI:

| Comment | Effect |
|---------|--------|
| `@mergifyio queue` | Add the PR to the merge queue (also use to **requeue** a PR that was dequeued) |
| `@mergifyio dequeue` | Remove (dequeue) the PR from the merge queue |

`@mergifyio requeue` is accepted, but it is a deprecated alias that runs the same command as `@mergifyio queue` — there is no separate requeue behavior. Post `@mergifyio queue`.

When Mergify processes the comment, it adds a 👍 (thumbs up) reaction to the comment to acknowledge receipt. After queuing, use `mergify queue show <PR_NUMBER>` to watch the PR's status as it progresses through the queue.

## Commands

```bash
mergify queue status                 # Show queue status (batches, waiting PRs)
mergify queue status --branch main   # Filter by branch
mergify queue status --json          # Machine-readable JSON output
mergify queue show <PR_NUMBER>       # Detailed state of a PR in the queue
mergify queue show <PR_NUMBER> -v    # Full checks table and conditions tree
mergify queue show <PR_NUMBER> --json # Machine-readable JSON output
mergify queue pause --reason "..."   # Pause the queue (requires reason)
mergify queue unpause                # Resume the queue
```

That is the whole `queue` group: `status`, `show`, `pause`, `unpause`. There is no subcommand for dequeuing a PR, none for queue history, and no flag that reports a dequeue reason.

## Is the PR queued, dequeued, or never queued?

Start here — the rest of the workflow branches on this answer.

`mergify queue show <PR>` asks the API for the PR's *current* queue entry. A PR with no entry is a normal answer, not an error: the command prints a notice and **exits 0**.

| `queue show` result | Meaning |
|---|---|
| `PR #N` block with position / CI state | The PR **is in the queue** |
| `PR #N is not in the merge queue` | The PR is **not in the queue** — dequeued, never queued, or not a real PR number |

In `--json`, the not-queued case is the only payload carrying a `queued` key, so it is an unambiguous test:

```bash
mergify queue show 1234 --json | jq -e '.queued == false' >/dev/null && echo "not in queue"
```

**`queue show` cannot tell "dequeued" from "never queued".** Both are the same 404 from the API and the same notice line. To separate them, look for a queue *history*. The discriminator is an `action.queue.leave` event: present means the PR was in the queue and left it, absent means it never got in. Get it from surface 1 below.

The command does not check that the PR exists, so a typo'd number prints the same notice. If the leave-event lookup also comes back empty, confirm the PR is real (`gh pr view <PR>`) before concluding anything.

Do not use the presence of a `Mergify Merge Queue` check run as the test — a PR that merely *matches* the queue conditions gets one titled `Waiting for queue conditions` without ever being queued. A `# Merge Queue Status` comment is a reliable positive signal (the pre-queue "Queue this pull request" offer comment deliberately carries no such heading), but its absence proves nothing, since the comment can be disabled per repository.

## Diagnosing a dequeued PR

Three surfaces carry the reason. Prefer them in this order.

### 1. The Mergify activity log (machine-readable, authoritative)

`GET /v1/repos/{owner}/{repo}/logs` returns the queue lifecycle events, newest first. **No CLI command exposes this** — call it directly:

```bash
REPO=owner/repo
PR=1234
FROM=$(date -u -d '90 days ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-90d +%Y-%m-%dT%H:%M:%SZ)

curl -sS -H "Authorization: Bearer ${MERGIFY_TOKEN:-$(gh auth token)}" \
  "https://api.mergify.com/v1/repos/$REPO/logs?pull_request=$PR&event_type=action.queue.leave&received_from=$FROM" \
| jq 'if .events == [] then "no leave event in window — never queued (or aged out)"
      else .events[0].metadata
           | {merged, dequeue_code, reason,
              failing: [.unsuccessful_checks[]? | {name, state, details_url}]}
      end'
```

```json
{
  "merged": false,
  "dequeue_code": "CHECKS_FAILED",
  "reason": "The merge conditions cannot be satisfied due to failing checks\n\n- `ci-gate`",
  "failing": [
    {
      "name": "ci-gate",
      "state": "failure",
      "details_url": "https://github.com/owner/repo/actions/runs/28589756829/job/84771312071"
    }
  ]
}
```

Read it as:

- `"events": []` / `size: 0` → no leave event in the window → the PR was **never queued** (or the event aged out; see the window rule below).
- `merged: true` → it left the queue **by merging**. Not a dequeue.
- `merged: false` → it was **dequeued**; `dequeue_code` says why (see the reason table).
- `unsuccessful_checks[].details_url` → **direct link to the failing CI job log**. This is how you reach the CI failure after the PR has left the queue.

Two traps that make this silently return nothing:

- **`received_from` is required in practice.** The window defaults to the *last 24 hours*. A dequeue from last week returns `size: 0` with no error, which reads exactly like "never queued". Always pass `received_from`.
- **The window may not exceed 93 days** (retention is 90 days) or the call fails with `422 'received_from' and 'received_to' cannot span more than 93 days`.

Same endpoint, other useful filters: `&outcome=failure` restricts leave events to dequeues (a merge is `success`); drop `event_type` to see the whole lifecycle (`action.queue.enter`, `checks_start`, `checks_end`, `leave`).

### 2. The `Mergify Merge Queue` check run

The check-run **title** names the reason directly, and its summary is the full queue report:

```bash
SHA=$(gh pr view $PR --repo $REPO --json headRefOid -q .headRefOid)
gh api "repos/$REPO/commits/$SHA/check-runs" \
  -q '.check_runs[] | select(.name=="Mergify Merge Queue") | {conclusion, title: .output.title, summary: .output.summary}'
```

Titles map to state without any parsing:

| `output.title` | State |
|---|---|
| `Dequeued — <reason>` (conclusion `neutral`) | Dequeued, reason in the title |
| `Dequeued from merge queue` (conclusion `neutral`) | Dequeued, but the reason did not resolve to a named code — use surface 1 |
| `Merged via merge queue` (conclusion `success`) | Merged by the queue |
| `Waiting for queue conditions`, `Checks …`, `In merge queue` | Still in the lifecycle |

Caveat: the check run lives on the **head SHA it was written against**. If the dequeue was caused by a push (`PULL_REQUEST_UPDATED`, `DRAFT_PULL_REQUEST_CHANGED`), the current head has a *fresh* check run and the dequeue report sits on the previous SHA. Use surface 1 or 3 in that case. Note also that `gh pr view --json statusCheckRollup` returns a null `title` — go through `gh api .../check-runs` as above.

### 3. The `# Merge Queue Status` comment

`mergify[bot]` posts one comment per queue session, so **read the last one**. It survives pushes, which makes it the most robust GitHub-side surface.

```bash
gh api --paginate --slurp "repos/$REPO/issues/$PR/comments" \
| jq -r '[.[][] | select(.user.login=="mergify[bot]")
              | select(.body|contains("# Merge Queue Status"))] | last | .body'
```

`--paginate` matters: the endpoint returns 30 comments per page and the newest are on the *last* page, so without it `last` silently hands you a stale status comment (or none) on any PR with real discussion. `--slurp` collects the pages into an array of arrays — hence `.[][]` to flatten — and is incompatible with `-q`, so the filter goes through `jq` instead.

Its structure, in order: a hidden JSON payload, a timeline, the merge conditions, then `## Reason`, `Failing checks:` (each with a `[job log]` link), and `## Hint`. The hidden payload gives the state without parsing prose:

```
<!--- ... {"version": 1, "state": "dequeued", "queue_rule_name": "default", ...} ... -->
```

`state` is one of `waiting`, `checking`, `frozen`, `bisecting`, `merged`, `dequeued`. It does **not** carry the dequeue code — that is in the `## Reason` prose below it.

Caveat: this comment can be turned off per repository (`merge_queue.status_comments: none`, or `outcomes` for terminal events only). Absence of a comment does not prove the PR was never queued.

## Dequeue reasons and what to do next

`dequeue_code` values and the action they call for. The engine ships a per-reason `## Hint` in the report — for a code not listed here, read that Hint rather than guessing.

| `dequeue_code` | What happened | What to do next |
|---|---|---|
| `PR_MERGED` | Merged by the queue | Nothing — this is success |
| `PR_MANUALLY_MERGED` | Merged outside the queue | Nothing |
| `CHECKS_FAILED` | Required checks failed on the merge commit | Read `unsuccessful_checks[].details_url`, fix the CI. Pushing a fix requeues it automatically once conditions match again; if it was flaky, requeue as-is with `@mergifyio queue` |
| `CHECKS_TIMEOUT` | `checks_timeout` elapsed before conditions were satisfied | Check the reason's details: checks that **never reported** mean the check names in your conditions don't match what CI publishes (fix the config, not the PR). Checks **still running** mean CI is too slow or stuck |
| `PULL_REQUEST_UPDATED` | Someone pushed to the PR while it was queued | Stop pushing to a queued PR. Requeue when the branch is final |
| `DRAFT_PULL_REQUEST_CHANGED` | The queue's draft/batch PR got commits Mergify did not create | Never push to the merge-queue draft branch. Requeue the original PR |
| `CONFLICT_WITH_BASE_BRANCH` | The PR conflicts with its base branch | Rebase or merge the base branch, resolve conflicts, then requeue |
| `CONFLICT_WITH_PULL_AHEAD` | The PR conflicts with a PR ahead of it in the queue | Wait for the PR ahead to merge, then rebase and requeue |
| `BRANCH_UPDATE_FAILED` | Mergify could not update the PR's head branch | Read the reason details, update the branch yourself, requeue |
| `BASE_BRANCH_MISSING` / `BASE_BRANCH_CHANGED` | The base branch is gone or changed | Retarget the PR to a live base branch, then requeue |
| `PR_MANUALLY_DEQUEUED` | A human removed it (command, dashboard, or API) | The reason names who and how. Requeue only once you know why they pulled it |
| `PR_DEQUEUED` | Queue conditions stopped matching | Look at the conditions in the report; fix the PR or requeue |
| `DROPPED_BY_BISECTION_ELIMINATION` | Bisection blamed other PRs and dropped this one untested | It is unproven, not known-broken. Requeue to test it on its own |
| `STACK_PREDECESSOR_DEQUEUED` | A predecessor in the same stack was dequeued | Fix the predecessor, requeue the stack |
| `QUEUE_RULE_MISSING` / `CONFIGURATION_CHANGED` | The config changed under the queued PR | Fix `.mergify.yml` (see the `mergify-config` skill), then requeue |
| `INCOMPATIBILITY_WITH_BRANCH_PROTECTIONS` | Queue settings clash with branch protections | Reconcile the repository's branch protections with the queue config — requeuing alone will not help |
| `UNPROCESSABLE_PULL_REQUEST` | Too many check runs, comments, or files for Mergify to process | Shrink the PR |

**Not every code means the PR left the queue.** These reasons interrupt the *checks* and the PR **stays queued** — do not treat them as a dequeue and do not requeue:

`PR_AHEAD_DEQUEUED`, `BATCH_AHEAD_FAILED`, `PR_WITH_HIGHER_PRIORITY_QUEUED`, `MERGE_QUEUE_RESET`, `SCHEDULED_FREEZE_STATUS_CHANGED`, `SPECULATIVE_CHECK_NUMBER_REDUCED`, `INTERMEDIATE_RESULTS_SKIPPED`, `CHECKS_RETRIED`, `BATCH_SCOPES_CHANGED`, `SCHEDULE_BLOCKED_AHEAD_YIELDED`, `PR_CHECKS_STOPPED_BECAUSE_MERGE_QUEUE_PAUSE`

They arrive as `abort_code` on an `action.queue.checks_end` event (with `aborted: true`) rather than as `dequeue_code` on a leave event, and the check-run title reads `Checks restarted — …` or `Checks aborted — …` rather than `Dequeued — …`. The authoritative test for "did it actually leave the queue" is an `action.queue.leave` event with `merged: false` — not the presence of a code from this list.

## Checking Queue Status

Use `mergify queue status` to see the current state of the merge queue:

- **Batches**: groups of PRs being tested together, shown with their CI status and ETA
- **Waiting PRs**: PRs queued but not yet in a batch, shown with priority and queue time
- **Pause state**: whether the queue is paused and why

Use `--json` when you need to parse the output programmatically.

## Inspecting a PR in the queue

Use `mergify queue show <PR_NUMBER>` to check why a PR is stuck or how it's progressing:

- **Position**: where the PR sits in the queue
- **Priority**: which priority rule matched
- **CI state**: whether checks are passing, pending, or failing
- **Conditions**: which conditions are met and which are blocking
- Use `-v` (verbose) for the full checks table and conditions tree

`-v` lists check **names and states only — no links to the CI jobs**. For job-log URLs, use the dequeue-report surfaces above (they apply to a queued PR too, via the check-run summary). `--json` is a raw passthrough of the API payload, so it carries two fields the human render drops: `checks_timeout_at` (when this PR will hit `CHECKS_TIMEOUT` — worth reading *before* it does) and `queue_rule` (the resolved queue rule config, not just its name).

## Queue States

| State | Meaning |
|-------|---------|
| `running` | Batch is actively running CI |
| `preparing` | Batch is being set up |
| `bisecting` | Batch failed, bisecting to find the culprit |
| `failed` | CI failed for this batch |
| `merged` | PRs in this batch have been merged |
| `waiting_for_merge` | CI passed, waiting for GitHub to merge |
| `waiting_for_previous_batches` | Blocked on earlier batches completing |
| `waiting_for_batch` | Waiting to be picked up into a batch |
| `waiting_for_requeue` | A batch ahead failed; this batch will be re-embarked |
| `waiting_schedule` | Outside the configured merge schedule |
| `frozen` | Queue is paused |

## Pausing and Unpausing

Pause the queue to temporarily halt all merges (e.g., during incidents or deployments):

```bash
mergify queue pause --reason "production incident — halting merges"
mergify queue unpause
```

- Pausing does **not** cancel running CI — it prevents new merges from starting
- The reason is visible to all team members in the queue status
- Use `--yes-i-am-sure` to skip the confirmation prompt in scripts

## Troubleshooting

**PR not entering the queue:**
- Make sure the PR was queued: post `@mergifyio queue` and confirm Mergify reacted with 👍 on the comment
- Check that the PR's merge conditions are met: `mergify queue show <PR_NUMBER> -v`
- Look at the conditions section for unmet requirements
- If `queue show` says the PR is not in the queue, it may have been queued and dequeued already — check for a leave event before assuming the queue command never landed

**PR stuck in queue:**
- Check CI state: `mergify queue show <PR_NUMBER>`
- If checks are failing, `-v` names them; for the job logs, read the `Failing checks:` links in the `# Merge Queue Status` comment or the `Mergify Merge Queue` check-run summary
- If the queue is paused, check who paused it: `mergify queue status`

**PR disappeared from the queue:**
- Do not conclude it was never queued — `mergify queue show` reports the same "not in the merge queue" for both cases
- Go to [Diagnosing a dequeued PR](#diagnosing-a-dequeued-pr): get `dequeue_code`, then act per the reason table

**Queue moving slowly:**
- Check for failing batches that trigger bisection: `mergify queue status`
- Bisecting batches test PRs individually, which is slower than batch merging
