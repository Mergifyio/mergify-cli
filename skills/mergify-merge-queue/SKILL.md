---
name: mergify-merge-queue
description: Use Mergify merge queue to queue/dequeue PRs and to monitor, inspect, pause, and manage the merge queue. ALWAYS use this skill when queuing or dequeuing a PR, checking queue status, investigating PR merge state, pausing/unpausing the queue, debugging merge failures, or asking why a PR was dequeued after it left the queue. Triggers on queue a PR, requeue, dequeue, merge queue, queue status, queue pause, queue show, queue history, why was my PR dequeued, which draft ran the checks, pause, unpause, frozen, bisecting, batch, CI checks.
---

# Mergify Merge Queue

## Overview

The merge queue serializes PR merges, running CI on temporary merge commits to catch integration failures before they reach the target branch. Use comments on the PR to queue/dequeue it, and the CLI to monitor queue state, inspect individual PRs, and manage the queue.

## Queuing and Dequeuing a PR

Queue, dequeue, and requeue actions are driven by **comments on the pull request**, not the CLI:

| Comment | Effect |
|---------|--------|
| `@mergifyio queue` | Add the PR to the merge queue (also use to **requeue** a PR that was dequeued) |
| `@mergifyio dequeue` | Remove (dequeue) the PR from the merge queue |

When Mergify processes the comment, it adds a 👍 (thumbs up) reaction to the comment to acknowledge receipt. After queuing, use `mergify queue show <PR_NUMBER>` to watch the PR's status as it progresses through the queue.

## Commands

```bash
mergify queue status                 # Show queue status (batches, waiting PRs)
mergify queue status --branch main   # Filter by branch
mergify queue status --json          # Machine-readable JSON output
mergify queue show <PR_NUMBER>       # Detailed state of a PR in the queue
mergify queue show <PR_NUMBER> -v    # Full checks table and conditions tree
mergify queue show <PR_NUMBER> --json # Machine-readable JSON output
mergify queue history <PR_NUMBER>    # Queue event trail, incl. after the PR left the queue
mergify queue history <PR_NUMBER> --json # Machine-readable JSON output
mergify queue pause --reason "..."   # Pause the queue (requires reason)
mergify queue unpause                # Resume the queue
```

`show` describes the **live** queue only: once a PR is dequeued or merged it
answers "PR #N is not in the merge queue". `history` is the one that still
answers afterwards.

## Checking Queue Status

Use `mergify queue status` to see the current state of the merge queue:

- **Batches**: groups of PRs being tested together, shown with their CI status and ETA
- **Waiting PRs**: PRs queued but not yet in a batch, shown with priority and queue time
- **Pause state**: whether the queue is paused and why

Use `--json` when you need to parse the output programmatically.

## Inspecting a PR

Use `mergify queue show <PR_NUMBER>` to check why a PR is stuck or how it's progressing:

- **Position**: where the PR sits in the queue
- **Priority**: which priority rule matched
- **CI state**: whether checks are passing, pending, or failing
- **Conditions**: which merge conditions are met and which are blocking
- Use `-v` (verbose) for the full checks table and conditions tree

## Reading a PR's Queue History

`mergify queue show` goes blank the moment a PR leaves the queue, which is
exactly when you need to know what happened.
`mergify queue history <PR_NUMBER>` replays the queue's own event trail,
oldest first:

- **queued / dequeued / merged** — when it entered, when it left, and the
  dequeue code (`CHECKS_FAILED`, `PR_AHEAD_DEQUEUED`, `PR_DEQUEUED`, …) plus
  the reason text
- **checks started / ended** — which **draft PR** ran the checks (the CI
  failure lives on that draft, not on the original PR), and which checks failed
  with their job URLs
- **batched with #X, #Y** — the other PRs in the same batch, so you can tell
  whether a failure is even this PR's fault
- **bisection started / ended** — when a failing batch was split, and which PRs
  were blamed

Two things to read carefully:

- **A PR can end its checks more than once.** A retried batch emits a
  `checks ended` with `CHECKS_RETRIED` *before* the real one. Take the **last**
  `checks ended`, not the first.
- **Fix the original PR, never the draft.** The draft is disposable; Mergify
  recreates it on requeue.

The trail covers the last **90 days** (Mergify's activity-log retention). An
empty trail means "the queue did not touch this PR in that window" — for an
older PR it does not mean nothing went wrong.

Under `--json` each event is a flat record: `type`, `received_at`, `outcome`,
`draft_pull_request`, `batched_pull_requests`, `checks_conclusion`,
`abort_code`, `dequeue_code`, `merged`, `unsuccessful_checks[].url`. The
document also reports the window queried and a `truncated` flag.

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

**PR stuck in queue:**
- Check CI state: `mergify queue show <PR_NUMBER>`
- If checks are failing, inspect the failing checks with `-v`
- If the queue is paused, check who paused it: `mergify queue status`

**PR was dequeued and you don't know why:**
- `mergify queue history <PR_NUMBER>` — `show` cannot answer once the PR is out
  of the queue
- Read the last `checks ended` for the draft PR that ran the checks and the
  failing check's job URL; the earlier ones may just be retries
- Check `batched with` before blaming this PR: a `PR_AHEAD_DEQUEUED` failure
  belongs to someone else's PR

**Queue moving slowly:**
- Check for failing batches that trigger bisection: `mergify queue status`
- Bisecting batches test PRs individually, which is slower than batch merging
