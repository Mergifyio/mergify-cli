---
name: mergify-events
description: Use `mergify events` to browse the Mergify activity log — every event Mergify recorded for a repository or one pull request (queue enters/leaves, merges, commands, CI Insights, freezes), as a human timeline or JSON. ALWAYS use this skill when investigating what Mergify did to a PR or repository and when, reconstructing a pull request's merge-queue lifecycle, auditing Mergify actions, or filtering events by type. Triggers on activity log, event log, events, what did Mergify do, queue lifecycle, queue history, event timeline, event_type, action.queue.
---

# Mergify Events

## Overview

`mergify events` lists the repository's Mergify activity log as a timeline — the ~45 event types the engine records: the `action.queue.*` lifecycle, workflow actions (`action.merge`, `action.rebase`, `action.label`, …), user commands (`command.queue`, `command.dequeue`), `ci_insights.*`, queue pauses, and scheduled freezes. One command over filters; there is deliberately no command per event type.

```bash
mergify events                                  # whole repo, last 24h
mergify events --pr 1740                        # one PR's events, last 24h
mergify events --pr 1740 --since 7d             # wider window (s/m/h/d/w, max 90d)
mergify events --type action.queue.leave --type command.queue   # filter, repeatable
mergify events --pr 1740 --json                 # raw events, newest first
mergify events --limit 20                       # newest 20 only (the header says so)
```

## The window rule (the one thing to get right)

Every result covers an **explicit time window**, stated in the header and in the empty-case message:

```
PR #1740 · 6 events · 2026-07-29 21:00 → 2026-07-30 21:00 UTC
```

- Default window: the **last 24 hours**. A PR dequeued last week shows **nothing** in the default window — that is "nothing in the last 24h", never "no history".
- Retention is **90 days**; `--since 90d` is the widest useful window. Anything wider is rejected up front with the fix in the message.
- An empty result names the window: `No events for PR #1740 between <from> and <to> UTC.` If you did not search the full retention yet, widen with `--since 90d` before concluding anything.

## Reading the timeline

Oldest first (it reads down the page), with a summary per event where the metadata carries one:

```
  2026-07-30
  14:02  action.queue.enter         default
  14:31  action.queue.checks_start  default · draft PR #1801
  15:04  action.queue.checks_end    CHECKS_RETRIED
  15:04  action.queue.leave         CHECKS_FAILED
  15:12  command.queue              @jd
```

Caveat that prevents a real mistake: an abort code on **`action.queue.checks_end`** (`PR_AHEAD_DEQUEUED`, `MERGE_QUEUE_RESET`, `CHECKS_RETRIED`, …) means the *checks* were interrupted while the PR **stayed queued**. Only **`action.queue.leave`** means the PR left the queue — and its `merged: true` variant means it left by merging. Do not requeue a PR over a `checks_end` event.

## JSON contract

`--json` emits one document; `events` are the API's raw objects (unknown fields intact), **newest first**:

```json
{
  "repository": "owner/repo",
  "pull_request": 1740,
  "received_from": "2026-07-29T21:00:00+00:00",
  "received_to": "2026-07-30T21:00:00+00:00",
  "size": 6,
  "events": [ { "id": 123, "type": "action.queue.leave", "received_at": "…", "metadata": { "…": "…" } } ]
}
```

The window is echoed so an empty `events` is self-describing. Filter with `jq` on `.events[].type` and `.events[].metadata`.

## When to use something else

- **"Why was this PR dequeued?"** — `mergify queue show <PR>` (the `mergify-merge-queue` skill) is the dedicated answer: it renders the last leave event with the reason, the failing checks' job URLs, and the head-SHA staleness check. `mergify events` is for the *whole* trail or for non-queue events.
- **Raw API access** (CLI not installed, or a token refused the log): `GET /v1/repos/{owner}/{repo}/logs` — same data; always pass `received_from` (the API silently defaults to 1 day) and keep the span ≤ 93 days.
