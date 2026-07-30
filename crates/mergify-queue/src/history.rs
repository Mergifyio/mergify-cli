//! `mergify queue history` — merge-queue event trail of a single
//! pull request, readable long after the pull request left the
//! queue.
//!
//! `queue show` and `GET /merge-queue/pull/<n>` only describe the
//! *live* queue: once a pull request is dequeued or merged they
//! answer "not in the merge queue" and the story of what happened is
//! gone. This command reads it back from the activity log,
//! `GET /v1/repos/<repo>/logs`, which keeps the queue's own events:
//! which draft ran the checks, which pull requests were batched into
//! it, which checks failed, and why the pull request was dequeued.
//!
//! Three decisions that are not obvious from the endpoint:
//!
//! - **The time range is always explicit.** `/logs` defaults
//!   `received_from` to `received_to - 1 day`, so the naive
//!   `?pull_request=N` request answers "no events" for anything that
//!   happened more than 24h ago — the silent false all-clear this
//!   command exists to prevent. We always ask for the server's full
//!   retention window ([`RETENTION_DAYS`]) so "no events" means "the
//!   queue never touched this pull request in the window", and the
//!   window is reported in the output so the caller can tell that
//!   apart from "it happened before retention".
//! - **The event types are filtered server-side.** The activity log
//!   also carries labels, comments and reviews, plus two queue events
//!   (`action.queue.change`, `action.queue.checks.change`) that only
//!   report the queue's *size* changing and say nothing about this
//!   pull request. Filtering server-side keeps the single page we
//!   fetch full of events that matter instead of noise.
//! - **`--json` is a normalized trail, not an API passthrough**
//!   (unlike `queue status` / `queue show`). The raw events are a
//!   per-type metadata union whose `action.queue.leave` member embeds
//!   the whole condition-evaluation tree — kilobytes of nesting a
//!   caller would have to re-dig on every event type. The point of
//!   the command is to spare callers Mergify's event schema, so the
//!   fields that matter are flattened onto each event. New fields may
//!   be added; existing ones are not renamed or removed.
//!
//! The command reports facts and draws no conclusion: it does not
//! decide *the* outcome, because a pull request can end its checks
//! more than once (a retried batch emits a first `checks_end` with
//! `abort_code: CHECKS_RETRIED` before the real one) and picking a
//! single "the" event is exactly how a caller ends up reading the
//! wrong attempt. Events come out oldest-first; the last one of a
//! given type is the latest attempt.
//!
//! Exit codes: `0` on any successful render, including an empty
//! trail — a pull request that never entered the queue is a normal
//! answer, not a failure. Standard `CliError` codes otherwise.

use std::io::Write;

use anstyle::AnsiColor;
use chrono::DateTime;
use chrono::SecondsFormat;
use chrono::Utc;
use mergify_core::CliError;
use mergify_core::CommandContext;
use mergify_core::Output;
use mergify_tui::StyledGlyph;
use mergify_tui::Theme;
use serde::Deserialize;
use serde::Serialize;

/// How far back the trail is fetched. Matches the engine's
/// `CLIENT_DATA_RETENTION_TIME`: the activity log keeps 90 days and
/// `/logs` rejects a window wider than retention + 3 days, so this is
/// simultaneously "everything Mergify still remembers" and the widest
/// range the endpoint accepts.
const RETENTION_DAYS: i64 = 90;

/// One page is all we fetch. 100 is the endpoint's maximum, and after
/// the event-type filter it is roughly twenty full queue cycles for a
/// single pull request — far more than any real trail. Going further
/// would mean following the RFC 5988 `Link` cursor, which the HTTP
/// client does not expose; instead the response is flagged
/// `truncated` so a caller is never silently shown a partial trail.
const PER_PAGE: usize = 100;

/// Queue event types requested from `/logs`.
///
/// An include-list rather than an exclude-list: the activity log is
/// repository-wide and mostly not about the merge queue. The cost is
/// that `/logs` validates the values against a closed enum, so a type
/// removed from the API would 422 the whole request — acceptable
/// because the set has only ever grown, and a wrong-but-present type
/// fails loudly instead of silently dropping events.
///
/// Deliberately excluded: `action.queue.change` and
/// `action.queue.checks.change`, which report the queue's size and
/// running-check count changing. They are attributed to whichever
/// pull request triggered the recomputation and carry nothing about
/// it.
const QUEUE_EVENT_TYPES: &[&str] = &[
    "action.queue.enter",
    "action.queue.checks_start",
    "action.queue.checks_end",
    "action.queue.checks_not_started",
    "action.queue.conflict_deferred",
    "action.queue.batch_bisection_start",
    "action.queue.batch_bisection_end",
    "action.queue.leave",
    "action.queue.merged",
    "action.dequeue",
    "command.queue",
    "command.dequeue",
];

pub struct HistoryOptions<'a> {
    pub repository: Option<&'a str>,
    pub token: Option<&'a str>,
    pub api_url: Option<&'a str>,
    pub pr_number: u64,
    pub output_json: bool,
}

// ---------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------

#[derive(Deserialize)]
struct LogsResponse {
    #[serde(default)]
    events: Vec<RawEvent>,
}

#[derive(Deserialize)]
struct RawEvent {
    id: i64,
    received_at: String,
    #[serde(default)]
    trigger: String,
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    outcome: Option<String>,
    // `Option` rather than `#[serde(default)]` alone: the event types
    // without metadata send it as an explicit `null`, which a plain
    // default would fail to deserialize.
    #[serde(default)]
    metadata: Option<RawMetadata>,
}

/// The union of every queue event's metadata, flattened.
///
/// `/logs` returns a discriminated union keyed on `type`, but a serde
/// enum over it would reject any event type the API adds later, and
/// `#[serde(other)]` cannot carry a payload. One optional-everything
/// struct deserializes every current and future member instead: a
/// field simply stays `None` on the event types that don't have it.
#[derive(Default, Deserialize)]
struct RawMetadata {
    #[serde(default)]
    queue_name: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    batch_id: Option<String>,
    #[serde(default)]
    batch_name: Option<String>,
    #[serde(default)]
    speculative_check_pull_request: Option<RawSpeculativeCheck>,
    #[serde(default)]
    aborted: Option<bool>,
    #[serde(default)]
    abort_code: Option<String>,
    #[serde(default)]
    retry_attempt: Option<u64>,
    #[serde(default)]
    max_retries: Option<u64>,
    #[serde(default)]
    merged: Option<bool>,
    #[serde(default)]
    dequeue_code: Option<String>,
    #[serde(default)]
    merge_commit_sha: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    unsuccessful_checks: Option<Vec<RawCheck>>,
    #[serde(default)]
    original_batch_pr_numbers: Option<Vec<u64>>,
    #[serde(default)]
    culprit_pr_numbers: Option<Vec<u64>>,
    #[serde(default)]
    blocking_pull_request_numbers: Option<Vec<u64>>,
}

/// Every field is optional, `number` included. The engine renames
/// keys inside this metadata during deprecation windows (see the
/// `draft_pr_number` → `batch_pr_number` rename this crate already
/// had to absorb), and one required field missing on one event of the
/// trail would fail deserialization of the whole page — costing the
/// caller the entire history rather than one field of one event. The
/// enclosing `EventBase` envelope (`id`, `received_at`, `type`) stays
/// required: it is the endpoint's stable contract, and a response
/// without it is not the response we asked for.
#[derive(Deserialize)]
struct RawSpeculativeCheck {
    #[serde(default)]
    number: Option<u64>,
    #[serde(default)]
    in_place: bool,
    #[serde(default)]
    checks_conclusion: Option<String>,
    #[serde(default)]
    checks_started_at: Option<String>,
    #[serde(default)]
    checks_ended_at: Option<String>,
    #[serde(default)]
    unsuccessful_checks: Vec<RawCheck>,
    #[serde(default)]
    pull_request_numbers: Vec<u64>,
}

#[derive(Deserialize)]
struct RawCheck {
    #[serde(default)]
    name: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

// ---------------------------------------------------------------
// Output types — this CLI's contract, additive only
// ---------------------------------------------------------------

#[derive(Serialize)]
struct History {
    repository: String,
    pull_request: u64,
    /// Start of the window queried, so an empty `events` can be told
    /// apart from activity that fell out of retention.
    received_from: String,
    received_to: String,
    /// `true` when the page came back full, which means older events
    /// were probably left behind: the trail is newest-complete,
    /// oldest-truncated. A trail of exactly one full page reports
    /// `true` with nothing actually dropped — the endpoint's cursor
    /// is in a `Link` header the HTTP client does not expose, so this
    /// errs toward warning rather than toward a silent partial trail.
    truncated: bool,
    /// Oldest first.
    events: Vec<Event>,
}

#[derive(Serialize)]
struct Event {
    id: i64,
    received_at: String,
    #[serde(rename = "type")]
    kind: String,
    outcome: String,
    trigger: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    queue_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    batch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    batch_name: Option<String>,
    /// The Mergify-opened draft pull request that ran the checks.
    /// `None` when `in_place` is true — the checks then ran on the
    /// pull request itself and there is no draft to look at.
    #[serde(skip_serializing_if = "Option::is_none")]
    draft_pull_request: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    in_place: Option<bool>,
    /// Every pull request the batch was checking, this one included.
    /// Answers "is this failure even mine?".
    #[serde(skip_serializing_if = "Option::is_none")]
    batched_pull_requests: Option<Vec<u64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    checks_conclusion: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    checks_started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    checks_ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    aborted: Option<bool>,
    /// Why the checks were cut short. `CHECKS_RETRIED` marks an
    /// attempt that was retried, not the pull request's fate.
    #[serde(skip_serializing_if = "Option::is_none")]
    abort_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_attempt: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_retries: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    merged: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dequeue_code: Option<String>,
    /// The commit the queue merge landed on the base branch.
    #[serde(skip_serializing_if = "Option::is_none")]
    merge_commit_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    /// Bisection: the whole batch that was split.
    #[serde(skip_serializing_if = "Option::is_none")]
    original_batch_pull_requests: Option<Vec<u64>>,
    /// Bisection: the pull requests blamed for the batch failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    culprit_pull_requests: Option<Vec<u64>>,
    /// `conflict_deferred`: the pull requests ahead in the queue that
    /// were in flight when the merge conflicted. The queue never
    /// learns which one owns the conflicting hunk.
    #[serde(skip_serializing_if = "Option::is_none")]
    blocking_pull_requests: Option<Vec<u64>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unsuccessful_checks: Vec<Check>,
}

#[derive(Serialize)]
struct Check {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
}

/// Run the `queue history` command.
pub async fn run(opts: HistoryOptions<'_>, output: &mut dyn Output) -> Result<(), CliError> {
    let ctx = CommandContext::resolve(opts.repository, opts.token, opts.api_url)?;
    let history = fetch(&ctx, opts.pr_number, Utc::now(), output).await?;

    if opts.output_json {
        let payload = serde_json::to_value(&history)
            .map_err(|e| CliError::wrap("serialize queue history", e))?;
        output.emit_json_value(&payload)?;
        return Ok(());
    }

    let theme = Theme::detect();
    output.emit(&history, &mut |w: &mut dyn Write| {
        render(w, &theme, &history)
    })?;
    Ok(())
}

/// Fetch and normalize the trail. `now` is a parameter rather than a
/// call to [`Utc::now`] so the queried window is deterministic in
/// tests.
async fn fetch(
    ctx: &CommandContext,
    pr_number: u64,
    now: DateTime<Utc>,
    output: &mut dyn Output,
) -> Result<History, CliError> {
    let received_to = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    let received_from =
        (now - chrono::Duration::days(RETENTION_DAYS)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let pr = pr_number.to_string();
    let per_page = PER_PAGE.to_string();

    let mut query: Vec<(&str, &str)> = vec![
        ("pull_request", pr.as_str()),
        ("per_page", per_page.as_str()),
        ("received_from", received_from.as_str()),
        ("received_to", received_to.as_str()),
    ];
    query.extend(QUEUE_EVENT_TYPES.iter().map(|t| ("event_type", *t)));

    output.status(&format!(
        "Fetching merge queue history for PR #{pr_number}…"
    ))?;

    let path = format!("/v1/repos/{repo}/logs", repo = ctx.repository);
    let response: LogsResponse = ctx.mergify_client()?.get_with_query(&path, &query).await?;

    let truncated = response.events.len() >= PER_PAGE;
    // `/logs` pages newest-first; a trail reads forward.
    let events = response.events.into_iter().rev().map(normalize).collect();

    Ok(History {
        repository: ctx.repository.clone(),
        pull_request: pr_number,
        received_from,
        received_to,
        truncated,
        events,
    })
}

fn normalize(raw: RawEvent) -> Event {
    let m = raw.metadata.unwrap_or_default();
    let spec = m.speculative_check_pull_request;
    let in_place = spec.as_ref().map(|s| s.in_place);

    // On an in-place check there is no draft: the checks ran on the
    // pull request itself, so reporting its own number under
    // `draft_pull_request` would send a caller looking for a draft
    // that does not exist.
    let draft_pull_request = spec.as_ref().filter(|s| !s.in_place).and_then(|s| s.number);

    // A `checks_end` carries its failing checks under the speculative
    // check; a `leave` carries them at the top level, already derived
    // from the conditions that blocked the merge. The two are
    // mutually exclusive today, but `filter` rather than a bare `or`
    // so that a speculative check with an empty list can never shadow
    // a populated top-level one if that ever changes.
    let unsuccessful_checks = spec
        .as_ref()
        .map(|s| s.unsuccessful_checks.as_slice())
        .filter(|checks| !checks.is_empty())
        .or(m.unsuccessful_checks.as_deref())
        .unwrap_or_default()
        .iter()
        .map(|c| Check {
            name: c.name.clone(),
            url: c.url.clone(),
            state: c.state.clone(),
        })
        .collect();

    Event {
        id: raw.id,
        received_at: raw.received_at,
        kind: raw.event_type,
        outcome: raw.outcome.unwrap_or_else(|| "neutral".to_string()),
        trigger: raw.trigger,
        queue_name: m.queue_name,
        branch: m.branch,
        batch_id: m.batch_id,
        batch_name: m.batch_name,
        draft_pull_request,
        in_place,
        batched_pull_requests: spec.as_ref().map(|s| s.pull_request_numbers.clone()),
        checks_conclusion: spec.as_ref().and_then(|s| s.checks_conclusion.clone()),
        checks_started_at: spec.as_ref().and_then(|s| s.checks_started_at.clone()),
        checks_ended_at: spec.as_ref().and_then(|s| s.checks_ended_at.clone()),
        aborted: m.aborted,
        abort_code: m.abort_code,
        retry_attempt: m.retry_attempt,
        max_retries: m.max_retries,
        merged: m.merged,
        dequeue_code: m.dequeue_code,
        merge_commit_sha: m.merge_commit_sha,
        reason: m.reason,
        original_batch_pull_requests: m.original_batch_pr_numbers,
        culprit_pull_requests: m.culprit_pr_numbers,
        blocking_pull_requests: m.blocking_pull_request_numbers,
        unsuccessful_checks,
    }
}

// ---------------------------------------------------------------
// Human rendering
// ---------------------------------------------------------------

/// Indent of an event's detail lines, including each failing check's
/// name.
const DETAIL_INDENT: &str = "      ";
/// Indent of a failing check's URL, one step under its name.
const SUB_DETAIL_INDENT: &str = "        ";

fn render(w: &mut dyn Write, theme: &Theme, history: &History) -> std::io::Result<()> {
    writeln!(
        w,
        "{B}Merge queue history for PR #{pr}{R} {D}· {repo}{R}",
        B = theme.bold,
        pr = history.pull_request,
        R = theme.reset,
        D = theme.dim,
        repo = history.repository,
    )?;
    writeln!(w)?;

    if history.events.is_empty() {
        // Say *which* window came back empty: a pull request that
        // left the queue longer ago than retention looks exactly like
        // one that was never queued, and the difference matters.
        writeln!(
            w,
            "  {D}No merge queue activity in the last {RETENTION_DAYS} days.{R}",
            D = theme.dim,
            R = theme.reset,
        )?;
        return Ok(());
    }

    // The queue and the branch are on every event and would repeat on
    // every row. State them only when they change from the last row
    // that stated them — so the first row shown always states them
    // (including when truncation dropped the entry that normally
    // would), and an event with no context of its own, such as a
    // metadata-less `action.dequeue`, does not make the next one
    // repeat what is already on screen.
    let mut stated: Option<(&str, &str)> = None;
    for event in &history.events {
        let context = event.queue_name.as_deref().zip(event.branch.as_deref());
        let restate = context.is_some() && context != stated;
        render_event(w, theme, event, history.pull_request, restate)?;
        if restate {
            stated = context;
        }
    }

    if history.truncated {
        writeln!(w)?;
        writeln!(
            w,
            "  {D}Showing the most recent {PER_PAGE} events; older activity omitted.{R}",
            D = theme.dim,
            R = theme.reset,
        )?;
    }
    Ok(())
}

fn render_event(
    w: &mut dyn Write,
    theme: &Theme,
    event: &Event,
    pull_request: u64,
    restate_context: bool,
) -> std::io::Result<()> {
    let glyph = outcome_glyph(theme, &event.outcome);
    writeln!(
        w,
        "  {D}{ts}{R}  {S}{icon}{R} {label}",
        D = theme.dim,
        ts = format_timestamp(&event.received_at),
        R = theme.reset,
        S = glyph.style,
        icon = glyph.icon,
        label = event_label(event),
    )?;

    for line in detail_lines(event, pull_request, restate_context) {
        writeln!(
            w,
            "{DETAIL_INDENT}{D}{line}{R}",
            D = theme.dim,
            R = theme.reset,
        )?;
    }

    for check in &event.unsuccessful_checks {
        writeln!(
            w,
            "{DETAIL_INDENT}{S}✗{R} {name}",
            S = theme.fg(AnsiColor::Red),
            R = theme.reset,
            name = check.name,
        )?;
        if let Some(url) = &check.url {
            writeln!(
                w,
                "{SUB_DETAIL_INDENT}{D}{url}{R}",
                D = theme.dim,
                R = theme.reset,
            )?;
        }
    }
    Ok(())
}

/// Human label for an event type. `action.queue.leave` is the one
/// type whose label depends on its payload: the same event ends both
/// a merge and a dequeue, and calling a merge "dequeued" would read
/// as a failure.
fn event_label(event: &Event) -> &str {
    match event.kind.as_str() {
        "action.queue.enter" => "queued",
        "action.queue.checks_start" => "checks started",
        "action.queue.checks_end" => "checks ended",
        "action.queue.checks_not_started" => "checks not started",
        "action.queue.conflict_deferred" => "conflict deferred",
        "action.queue.batch_bisection_start" => "bisection started",
        "action.queue.batch_bisection_end" => "bisection ended",
        "action.queue.leave" => {
            if event.merged == Some(true) {
                "left queue (merged)"
            } else {
                "dequeued"
            }
        }
        "action.queue.merged" => "merged",
        "action.dequeue" => "dequeue action",
        "command.queue" => "queue command",
        "command.dequeue" => "dequeue command",
        // A type the API added since this list was written still
        // shows up, under its raw name, rather than being dropped.
        other => other,
    }
}

/// The detail lines shown under an event, in reading order. Empty
/// when the event carries nothing beyond its label — except that the
/// trigger is then shown instead, so a bare `command.dequeue` still
/// says who asked for it.
fn detail_lines(event: &Event, pull_request: u64, restate_context: bool) -> Vec<String> {
    let mut lines = Vec::new();

    let mut facts: Vec<String> = Vec::new();
    if let Some(draft) = event.draft_pull_request {
        facts.push(format!("draft #{draft}"));
    } else if event.in_place == Some(true) {
        facts.push("checked in place".to_string());
    }
    if let Some(name) = &event.batch_name {
        facts.push(format!("batch {name}"));
    }
    if restate_context {
        if let Some(queue) = &event.queue_name {
            facts.push(format!("queue {queue}"));
        }
        if let Some(branch) = &event.branch {
            facts.push(format!("branch {branch}"));
        }
    }
    // A `checks_start` also carries a conclusion, but it is the one
    // the checks have *at* the start — always `pending`, never news.
    if event.kind == "action.queue.checks_end"
        && let Some(conclusion) = &event.checks_conclusion
    {
        facts.push(format!("checks {conclusion}"));
    }
    if let (Some(attempt), Some(max)) = (event.retry_attempt, event.max_retries) {
        facts.push(format!("retry {attempt}/{max}"));
    }
    if let Some(code) = event.abort_code.as_ref().or(event.dequeue_code.as_ref()) {
        facts.push(code.clone());
    }
    if !facts.is_empty() {
        lines.push(facts.join(" · "));
    }

    // Only the *other* pull requests in the batch, and only when
    // there are any: a batch of one needs no line, and listing the
    // pull request we were asked about back to the caller answers
    // nothing. This is the line that says whether a failure here can
    // even be this pull request's fault.
    let others: Vec<u64> = event
        .batched_pull_requests
        .iter()
        .flatten()
        .copied()
        .filter(|n| *n != pull_request)
        .collect();
    if !others.is_empty() {
        lines.push(format!("batched with {}", join_pr_numbers(&others)));
    }
    for (label, prs) in [
        ("batch", &event.original_batch_pull_requests),
        ("culprits", &event.culprit_pull_requests),
        ("blocked by", &event.blocking_pull_requests),
    ] {
        if let Some(prs) = prs.as_ref().filter(|p| !p.is_empty()) {
            lines.push(format!("{label} {}", join_pr_numbers(prs)));
        }
    }

    if let Some(sha) = &event.merge_commit_sha {
        lines.push(format!("merged as {sha}"));
    }

    // The dequeue reason is Mergify's own rendered markdown and can
    // carry emoji, from the user's Merge Protections rule names. It
    // goes through verbatim: the no-emoji rule guards *our* symbol
    // vocabulary and the column alignment it feeds, and this block is
    // free-form prose in no column. Rewriting a customer's rule name
    // would make the answer less useful, not more readable.
    if let Some(reason) = &event.reason {
        lines.extend(
            reason
                .lines()
                .map(str::trim_end)
                .filter(|l| !l.is_empty())
                .map(ToString::to_string),
        );
    }

    // An event with no metadata of its own — `action.dequeue`,
    // `command.dequeue` — has its trigger as the only record of who
    // or what caused it, and that is the question the row exists to
    // answer. Show it rather than an empty row.
    if lines.is_empty() && !event.trigger.is_empty() {
        lines.push(event.trigger.clone());
    }
    lines
}

fn join_pr_numbers(numbers: &[u64]) -> String {
    numbers
        .iter()
        .map(|n| format!("#{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Render `received_at` as an absolute UTC timestamp. The trail's
/// events land seconds apart, which the relative formatter used
/// elsewhere collapses to the same string on every row; a log has to
/// distinguish them, and an absolute time is also what correlates
/// with a CI run. Unparseable input falls back to the raw string so
/// the row still says something.
fn format_timestamp(raw: &str) -> String {
    DateTime::parse_from_rfc3339(raw).map_or_else(
        |_| raw.to_string(),
        |dt| {
            dt.with_timezone(&Utc)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        },
    )
}

/// Map the server-derived `outcome` to the single-width terminal
/// vocabulary. Unknown values fall back to a dim `—` so a new outcome
/// never breaks the render.
fn outcome_glyph(theme: &Theme, outcome: &str) -> StyledGlyph {
    match outcome {
        "success" => StyledGlyph::new("✓", theme.fg(AnsiColor::Green)),
        "failure" => StyledGlyph::new("✗", theme.fg(AnsiColor::Red)),
        "pending" => StyledGlyph::new("●", theme.fg(AnsiColor::Yellow)),
        "neutral" => StyledGlyph::new("○", theme.dim),
        _ => StyledGlyph::new("—", theme.dim),
    }
}

#[cfg(test)]
mod tests {
    use mergify_core::OutputMode;
    use mergify_test_support::Captured;
    use serde_json::json;
    use wiremock::Match;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::Request;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::header;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;

    /// Asserts the two query decisions the answer's trustworthiness
    /// rests on: the full retention window (`/logs` otherwise defaults
    /// to the last 24h and reports an older dequeue as "no events"),
    /// and the queue event-type filter.
    struct QueryContract;

    impl Match for QueryContract {
        fn matches(&self, request: &Request) -> bool {
            let pairs: Vec<(String, String)> = request
                .url
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            let first = |key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());

            let (Some(from), Some(to)) = (first("received_from"), first("received_to")) else {
                return false;
            };
            let (Ok(from), Ok(to)) = (
                DateTime::parse_from_rfc3339(&from),
                DateTime::parse_from_rfc3339(&to),
            ) else {
                return false;
            };

            let types: Vec<&str> = pairs
                .iter()
                .filter(|(k, _)| k == "event_type")
                .map(|(_, v)| v.as_str())
                .collect();

            (to - from).num_days() == RETENTION_DAYS
                && first("pull_request").as_deref() == Some("123")
                && first("per_page") == Some(PER_PAGE.to_string())
                && types == QUEUE_EVENT_TYPES
        }
    }

    fn check(url: &str) -> serde_json::Value {
        json!({"name": "all-greens", "description": "", "state": "failure",
               "url": url, "avatar_url": null})
    }

    fn checks_start(id: u64, at: &str, draft: u64) -> serde_json::Value {
        json!({
            "id": id, "received_at": at, "trigger": "merge queue internal",
            "repository": "owner/repo", "pull_request": 123, "base_ref": "main",
            "outcome": "pending", "type": "action.queue.checks_start",
            "metadata": {
                "branch": "main", "queue_name": "default",
                "batch_id": "d41f5ecf-b9d7-4616-a97a-182c563d620e",
                "batch_name": "assured-couronne",
                "speculative_check_pull_request": {
                    "number": draft, "in_place": false, "checks_timed_out": false,
                    "checks_conclusion": "pending",
                    "checks_started_at": null, "checks_ended_at": null,
                    "unsuccessful_checks": [], "pull_request_numbers": [123, 456],
                },
            },
        })
    }

    /// One `checks_end` attempt. A struct rather than nine
    /// positional arguments: the retried attempt and the final one
    /// differ in six of them, and reading them by name at the call
    /// site is the point of the fixture.
    struct Attempt<'a> {
        id: u64,
        at: &'a str,
        draft: u64,
        conclusion: &'a str,
        abort_code: &'a str,
        retry: Option<u64>,
        started_at: &'a str,
        ended_at: &'a str,
        url: &'a str,
    }

    fn checks_end(a: &Attempt<'_>) -> serde_json::Value {
        json!({
            "id": a.id, "received_at": a.at, "trigger": "merge queue internal",
            "repository": "owner/repo", "pull_request": 123, "base_ref": "main",
            "outcome": "failure", "type": "action.queue.checks_end",
            "metadata": {
                "aborted": true, "abort_code": a.abort_code,
                "branch": "main", "queue_name": "default",
                "retry_attempt": a.retry, "max_retries": a.retry,
                "batch_id": "d41f5ecf-b9d7-4616-a97a-182c563d620e",
                "batch_name": "assured-couronne",
                "speculative_check_pull_request": {
                    "number": a.draft, "in_place": false, "checks_timed_out": false,
                    "checks_conclusion": a.conclusion,
                    "checks_started_at": a.started_at, "checks_ended_at": a.ended_at,
                    "unsuccessful_checks": [check(a.url)],
                    "pull_request_numbers": [123, 456],
                },
            },
        })
    }

    /// One full queue cycle for PR #123: queued, checked on draft
    /// #900 alongside #456, retried on draft #901, failed, dequeued.
    /// Newest-first, the order `/logs` returns.
    fn logs_response() -> serde_json::Value {
        let events = vec![
            json!({
                "id": 6, "received_at": "2026-07-30T14:03:40.265869Z",
                "trigger": "merge queue internal", "repository": "owner/repo",
                "pull_request": 123, "base_ref": "main", "outcome": "failure",
                "type": "action.queue.leave",
                "metadata": {
                    "reason": "The merge conditions cannot be satisfied due to failing checks\n\n- `all-greens`",
                    "merged": false, "queue_name": "default", "branch": "main",
                    "dequeue_code": "CHECKS_FAILED",
                    "batch_id": "d41f5ecf-b9d7-4616-a97a-182c563d620e",
                    // Present on the wire and deliberately dropped:
                    // kilobytes of nesting no caller of this command
                    // asked for.
                    "conditions_evaluation": {"match": false, "label": "all of", "subconditions": []},
                    "unsuccessful_checks": [check("https://example.test/job/2")],
                },
            }),
            checks_end(&Attempt {
                id: 5,
                at: "2026-07-30T14:03:23.780681Z",
                draft: 901,
                conclusion: "failure",
                abort_code: "CHECKS_FAILED",
                retry: None,
                started_at: "2026-07-30T13:53:58.275846Z",
                ended_at: "2026-07-30T14:03:23.644589Z",
                url: "https://example.test/job/2",
            }),
            checks_start(4, "2026-07-30T13:53:58.912342Z", 901),
            checks_end(&Attempt {
                id: 3,
                at: "2026-07-30T13:53:43.694211Z",
                draft: 900,
                conclusion: "pending",
                abort_code: "CHECKS_RETRIED",
                retry: Some(1),
                started_at: "2026-07-30T13:42:30.078828Z",
                ended_at: "2026-07-30T13:53:43.633837Z",
                url: "https://example.test/job/1",
            }),
            checks_start(2, "2026-07-30T13:42:31.979443Z", 900),
            json!({
                "id": 1, "received_at": "2026-07-30T13:42:10.816003Z",
                "trigger": "Rule: auto_merge", "repository": "owner/repo",
                "pull_request": 123, "base_ref": "main", "outcome": "pending",
                "type": "action.queue.enter",
                "metadata": {
                    "queue_name": "default", "branch": "main",
                    "queued_at": "2026-07-30T13:42:07.037061Z",
                    "priority_rule_name": "<default>",
                },
            }),
        ];
        json!({"size": events.len(), "per_page": 100, "events": events})
    }

    async fn arrange(server: &MockServer, body: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/logs"))
            .and(header("Authorization", "Bearer t"))
            .and(QueryContract)
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(server)
            .await;
    }

    async fn run_against(server: &MockServer, cap: &mut Captured, output_json: bool) {
        let api_url = server.uri();
        run(
            HistoryOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                pr_number: 123,
                output_json,
            },
            &mut cap.output,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn run_renders_the_trail_oldest_first() {
        let server = MockServer::start().await;
        arrange(&server, logs_response()).await;

        let mut cap = Captured::human();
        run_against(&server, &mut cap, false).await;
        let stdout = cap.stdout();

        assert!(
            stdout.contains("Merge queue history for PR #123"),
            "got: {stdout:?}",
        );
        // Oldest first: the entry precedes the dequeue.
        let queued = stdout.find("queued").unwrap();
        let dequeued = stdout.find("dequeued").unwrap();
        assert!(queued < dequeued, "got: {stdout:?}");

        // Both attempts are shown, each against its own draft, and
        // the retried one is labelled as a retry rather than as the
        // pull request's fate.
        assert!(stdout.contains("draft #900"), "got: {stdout:?}");
        assert!(stdout.contains("draft #901"), "got: {stdout:?}");
        assert!(stdout.contains("retry 1/1"), "got: {stdout:?}");
        assert!(stdout.contains("CHECKS_RETRIED"), "got: {stdout:?}");
        assert!(stdout.contains("CHECKS_FAILED"), "got: {stdout:?}");

        // The failing check and its job URL, so the reader can go
        // straight to the failure.
        assert!(stdout.contains("all-greens"), "got: {stdout:?}");
        assert!(
            stdout.contains("https://example.test/job/2"),
            "got: {stdout:?}",
        );

        // The other pull request in the batch is named; this one is
        // not listed back to the caller.
        assert!(stdout.contains("batched with #456"), "got: {stdout:?}");
        assert!(!stdout.contains("#123, #456"), "got: {stdout:?}");

        // Absolute timestamps: relative ones would collapse the
        // seconds-apart rows onto the same string.
        assert!(stdout.contains("2026-07-30 13:42:10"), "got: {stdout:?}");

        // The queue and branch are stated once, on the first row,
        // not repeated on every one.
        assert_eq!(
            stdout.matches("queue default").count(),
            1,
            "got: {stdout:?}"
        );
    }

    #[tokio::test]
    async fn run_emits_a_normalized_json_trail() {
        let server = MockServer::start().await;
        arrange(&server, logs_response()).await;

        let mut cap = Captured::new(OutputMode::Json);
        run_against(&server, &mut cap, true).await;

        let stdout = cap.stdout();
        let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(parsed["repository"], json!("owner/repo"));
        assert_eq!(parsed["pull_request"], json!(123));
        assert_eq!(parsed["truncated"], json!(false));

        // The window is reported so an empty trail can be told apart
        // from activity that fell out of retention.
        let from = DateTime::parse_from_rfc3339(parsed["received_from"].as_str().unwrap()).unwrap();
        let to = DateTime::parse_from_rfc3339(parsed["received_to"].as_str().unwrap()).unwrap();
        assert_eq!((to - from).num_days(), RETENTION_DAYS);

        // Every field a caller needs, flattened out of the API's
        // per-type metadata union, oldest first.
        assert_eq!(
            parsed["events"],
            json!([
                {
                    "id": 1, "received_at": "2026-07-30T13:42:10.816003Z",
                    "type": "action.queue.enter", "outcome": "pending",
                    "trigger": "Rule: auto_merge",
                    "queue_name": "default", "branch": "main",
                },
                {
                    "id": 2, "received_at": "2026-07-30T13:42:31.979443Z",
                    "type": "action.queue.checks_start", "outcome": "pending",
                    "trigger": "merge queue internal",
                    "queue_name": "default", "branch": "main",
                    "batch_id": "d41f5ecf-b9d7-4616-a97a-182c563d620e",
                    "batch_name": "assured-couronne",
                    "draft_pull_request": 900, "in_place": false,
                    "batched_pull_requests": [123, 456],
                    "checks_conclusion": "pending",
                },
                {
                    "id": 3, "received_at": "2026-07-30T13:53:43.694211Z",
                    "type": "action.queue.checks_end", "outcome": "failure",
                    "trigger": "merge queue internal",
                    "queue_name": "default", "branch": "main",
                    "batch_id": "d41f5ecf-b9d7-4616-a97a-182c563d620e",
                    "batch_name": "assured-couronne",
                    "draft_pull_request": 900, "in_place": false,
                    "batched_pull_requests": [123, 456],
                    "checks_conclusion": "pending",
                    "checks_started_at": "2026-07-30T13:42:30.078828Z",
                    "checks_ended_at": "2026-07-30T13:53:43.633837Z",
                    "aborted": true, "abort_code": "CHECKS_RETRIED",
                    "retry_attempt": 1, "max_retries": 1,
                    "unsuccessful_checks": [
                        {"name": "all-greens", "state": "failure",
                         "url": "https://example.test/job/1"},
                    ],
                },
                {
                    "id": 4, "received_at": "2026-07-30T13:53:58.912342Z",
                    "type": "action.queue.checks_start", "outcome": "pending",
                    "trigger": "merge queue internal",
                    "queue_name": "default", "branch": "main",
                    "batch_id": "d41f5ecf-b9d7-4616-a97a-182c563d620e",
                    "batch_name": "assured-couronne",
                    "draft_pull_request": 901, "in_place": false,
                    "batched_pull_requests": [123, 456],
                    "checks_conclusion": "pending",
                },
                {
                    "id": 5, "received_at": "2026-07-30T14:03:23.780681Z",
                    "type": "action.queue.checks_end", "outcome": "failure",
                    "trigger": "merge queue internal",
                    "queue_name": "default", "branch": "main",
                    "batch_id": "d41f5ecf-b9d7-4616-a97a-182c563d620e",
                    "batch_name": "assured-couronne",
                    "draft_pull_request": 901, "in_place": false,
                    "batched_pull_requests": [123, 456],
                    "checks_conclusion": "failure",
                    "checks_started_at": "2026-07-30T13:53:58.275846Z",
                    "checks_ended_at": "2026-07-30T14:03:23.644589Z",
                    "aborted": true, "abort_code": "CHECKS_FAILED",
                    "unsuccessful_checks": [
                        {"name": "all-greens", "state": "failure",
                         "url": "https://example.test/job/2"},
                    ],
                },
                {
                    "id": 6, "received_at": "2026-07-30T14:03:40.265869Z",
                    "type": "action.queue.leave", "outcome": "failure",
                    "trigger": "merge queue internal",
                    "queue_name": "default", "branch": "main",
                    "batch_id": "d41f5ecf-b9d7-4616-a97a-182c563d620e",
                    "merged": false, "dequeue_code": "CHECKS_FAILED",
                    "reason": "The merge conditions cannot be satisfied due to failing checks\n\n- `all-greens`",
                    "unsuccessful_checks": [
                        {"name": "all-greens", "state": "failure",
                         "url": "https://example.test/job/2"},
                    ],
                },
            ]),
        );
    }

    #[tokio::test]
    async fn run_empty_trail_names_the_window_and_succeeds() {
        // A pull request the queue never touched is a normal answer,
        // not a failure — but the reply has to say how far back it
        // looked, or "no events" reads as "nothing went wrong".
        let server = MockServer::start().await;
        arrange(&server, json!({"size": 0, "per_page": 100, "events": []})).await;

        let mut cap = Captured::human();
        run_against(&server, &mut cap, false).await;

        let stdout = cap.stdout();
        assert!(
            stdout.contains("No merge queue activity in the last 90 days."),
            "got: {stdout:?}",
        );
    }

    #[tokio::test]
    async fn run_flags_a_truncated_trail() {
        // A full page means older events were left behind. Saying so
        // is the difference between a partial trail and a wrong one.
        let server = MockServer::start().await;
        let events: Vec<serde_json::Value> = (0..PER_PAGE)
            .map(|i| {
                json!({
                    "id": i, "received_at": "2026-07-30T13:42:10.816003Z",
                    "trigger": "Rule: auto_merge", "repository": "owner/repo",
                    "pull_request": 123, "base_ref": "main", "outcome": "pending",
                    "type": "action.queue.enter",
                    "metadata": {"queue_name": "default", "branch": "main"},
                })
            })
            .collect();
        arrange(
            &server,
            json!({"size": PER_PAGE, "per_page": 100, "events": events}),
        )
        .await;

        let mut cap = Captured::new(OutputMode::Json);
        run_against(&server, &mut cap, true).await;

        let parsed: serde_json::Value = serde_json::from_str(&cap.stdout()).unwrap();
        assert_eq!(parsed["truncated"], json!(true));
        assert_eq!(parsed["events"].as_array().unwrap().len(), PER_PAGE);
    }

    #[tokio::test]
    async fn run_states_the_queue_on_the_first_row_even_without_an_entry() {
        // Truncation drops the oldest events, so the `enter` that
        // would normally carry the queue and branch can be missing.
        // The first row shown has to state them anyway.
        let server = MockServer::start().await;
        let events = vec![checks_start(2, "2026-07-30T13:42:31.979443Z", 900)];
        arrange(
            &server,
            json!({"size": 1, "per_page": 100, "events": events}),
        )
        .await;

        let mut cap = Captured::human();
        run_against(&server, &mut cap, false).await;

        let stdout = cap.stdout();
        assert!(stdout.contains("queue default"), "got: {stdout:?}");
        assert!(stdout.contains("branch main"), "got: {stdout:?}");
    }

    #[tokio::test]
    async fn run_tolerates_an_event_with_null_metadata() {
        // `action.dequeue` and friends carry no metadata; the API
        // sends an explicit `null` there.
        let server = MockServer::start().await;
        arrange(
            &server,
            json!({
                "size": 1, "per_page": 100,
                "events": [{
                    "id": 1, "received_at": "2026-07-30T13:42:10.816003Z",
                    "trigger": "Rule: dequeue on conflict", "repository": "owner/repo",
                    "pull_request": 123, "base_ref": "main", "outcome": "neutral",
                    "type": "action.dequeue", "metadata": null,
                }],
            }),
        )
        .await;

        let mut cap = Captured::human();
        run_against(&server, &mut cap, false).await;

        let stdout = cap.stdout();
        assert!(stdout.contains("dequeue action"), "got: {stdout:?}");
        // With no metadata, the trigger is the only record of what
        // caused the dequeue, and the row exists to answer that.
        assert!(
            stdout.contains("Rule: dequeue on conflict"),
            "got: {stdout:?}",
        );
    }

    #[tokio::test]
    async fn run_does_not_restate_the_queue_after_a_metadata_less_event() {
        // A metadata-less event in the middle of a trail carries no
        // queue or branch. It must not make the next event repeat
        // what is already on screen.
        let server = MockServer::start().await;
        let events = vec![
            checks_start(3, "2026-07-30T13:44:00.000000Z", 900),
            json!({
                "id": 2, "received_at": "2026-07-30T13:43:00.000000Z",
                "trigger": "Rule: dequeue on conflict", "repository": "owner/repo",
                "pull_request": 123, "base_ref": "main", "outcome": "neutral",
                "type": "action.dequeue", "metadata": null,
            }),
            checks_start(1, "2026-07-30T13:42:31.979443Z", 900),
        ];
        arrange(
            &server,
            json!({"size": 3, "per_page": 100, "events": events}),
        )
        .await;

        let mut cap = Captured::human();
        run_against(&server, &mut cap, false).await;

        let stdout = cap.stdout();
        assert_eq!(
            stdout.matches("queue default").count(),
            1,
            "got: {stdout:?}",
        );
    }

    #[test]
    fn normalize_reports_no_draft_for_an_in_place_check() {
        // In-place checks run on the pull request itself; naming it
        // as the "draft" would send a reader after a draft that does
        // not exist.
        let raw: RawEvent = serde_json::from_value(json!({
            "id": 1, "received_at": "2026-07-30T13:42:31.979443Z",
            "trigger": "merge queue internal", "outcome": "pending",
            "type": "action.queue.checks_start",
            "metadata": {
                "speculative_check_pull_request": {
                    "number": 123, "in_place": true, "checks_conclusion": "pending",
                    "unsuccessful_checks": [], "pull_request_numbers": [123],
                },
            },
        }))
        .unwrap();

        let event = normalize(raw);
        assert_eq!(event.draft_pull_request, None);
        assert_eq!(event.in_place, Some(true));
        assert_eq!(
            detail_lines(&event, 123, false),
            vec!["checked in place".to_string()]
        );
    }

    #[test]
    fn event_label_tells_a_merge_from_a_dequeue() {
        // The same `action.queue.leave` event ends both; calling a
        // merge "dequeued" would read as a failure.
        let mut event = normalize(
            serde_json::from_value(json!({
                "id": 1, "received_at": "2026-07-30T14:03:40.265869Z",
                "trigger": "merge queue internal", "outcome": "failure",
                "type": "action.queue.leave",
                "metadata": {"merged": false},
            }))
            .unwrap(),
        );
        assert_eq!(event_label(&event), "dequeued");
        event.merged = Some(true);
        assert_eq!(event_label(&event), "left queue (merged)");
    }

    #[test]
    fn event_label_falls_back_to_the_raw_type() {
        // An event type added to the API after this list was written
        // still shows up rather than being silently dropped.
        let event = normalize(
            serde_json::from_value(json!({
                "id": 1, "received_at": "2026-07-30T14:03:40.265869Z",
                "trigger": "t", "outcome": "neutral",
                "type": "action.queue.something_new", "metadata": null,
            }))
            .unwrap(),
        );
        assert_eq!(event_label(&event), "action.queue.something_new");
    }

    #[test]
    fn format_timestamp_falls_back_to_the_raw_string() {
        assert_eq!(
            format_timestamp("2026-07-30T14:03:23.780681Z"),
            "2026-07-30 14:03:23",
        );
        assert_eq!(format_timestamp("not a date"), "not a date");
    }
}
