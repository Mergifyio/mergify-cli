//! `mergify events` — browse the repository's activity log as a
//! timeline, repo-wide or for one pull request.
//!
//! One command over a filter covers all ~45 event types; there is
//! deliberately no command per type. Two details carry the lesson
//! the `/logs` contract taught `queue show` (see the crate docs):
//! the header **always states the window**, so an empty result reads
//! as "nothing in the last 24 hours" and never as "nothing ever" —
//! that ambiguity is the bug this command removes — and the empty
//! case says so explicitly, naming the range and the retention.
//!
//! What the default page costs is the other half. This is a
//! low-level query over the API — filtering is `--pr` and `--type`,
//! not a curated feed — so the only thing the default owes the caller
//! is a bound: [`DEFAULT_LIMIT`] events, which is one API page, and a
//! header that says when that bound bit.
//!
//! Two output modes:
//!
//! - Human (default): a header naming the scope, the event count and
//!   the exact UTC window, then one line per event, **oldest first**
//!   (a timeline reads down the page), with a dim date row wherever
//!   the calendar date changes. Each line names the pull request it
//!   belongs to, and colors its outcome.
//! - `--json`: a single document with the query echoed
//!   (`repository`, `pull_request`, `received_from`, `received_to`),
//!   `size`, `truncated`, and `events` — the raw API events **newest
//!   first**, unknown fields intact, matching the client's ordering
//!   guarantee.

use std::io::Write;

use anstyle::Style;
use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;
use mergify_core::CliError;
use mergify_core::CommandContext;
use mergify_core::Output;
use mergify_tui::Theme;
use serde_json::Value;

use crate::client;
use crate::client::Query;
use crate::event::Event;
use crate::window::MAX_SPAN_DAYS;
use crate::window::RETENTION_DAYS;
use crate::window::Window;

/// The window used when `--since` is not given. Matches the API's
/// own default — but stated in the output instead of silently
/// applied, which is the difference that matters.
const DEFAULT_SINCE: TimeDelta = TimeDelta::hours(24);

/// How many events the default invocation fetches — exactly one API
/// page.
///
/// The window alone is not a budget: a busy repository records
/// thousands of events a day, so `--since 24h` with no cap walked
/// every page (100 at a time) before printing a line, and the no-flag
/// command took minutes. One page is one request, and — this is the
/// part that makes a default cap acceptable — the header says it is
/// the *newest* N and names `--limit` as the way past it. Nothing is
/// hidden, only deferred.
pub const DEFAULT_LIMIT: usize = 100;

pub struct ListOptions<'a> {
    pub repository: Option<&'a str>,
    pub token: Option<&'a str>,
    pub api_url: Option<&'a str>,
    /// Only this pull request's events; `None` covers the repository.
    pub pr_number: Option<u64>,
    /// How far back to look; `None` applies [`DEFAULT_SINCE`].
    pub since: Option<TimeDelta>,
    /// `event_type` filters, passed to the API verbatim (repeatable,
    /// OR semantics); empty selects every type.
    pub event_types: Vec<String>,
    /// Stop after the newest N events; the CLI defaults it to
    /// [`DEFAULT_LIMIT`]. The header says so when it takes effect.
    pub limit: usize,
    pub output_json: bool,
}

/// Parse a `--since` duration: an integer with a unit — `s`, `m`,
/// `h`, `d`, or `w` (e.g. `30m`, `12h`, `7d`).
///
/// The span cap is enforced here so an impossible window dies as a
/// usage error with the fix in it, not as an API 422 later.
///
/// # Errors
///
/// A message suitable for clap's `invalid value` report.
pub fn parse_since(value: &str) -> Result<TimeDelta, String> {
    let value = value.trim();
    let (number, unit) = value.split_at(value.len().saturating_sub(1));
    let count: i64 =
        number.parse().ok().filter(|n| *n > 0).ok_or_else(|| {
            "expected a positive integer with a unit, e.g. 30m, 12h, 7d".to_string()
        })?;
    let span = match unit {
        "s" => TimeDelta::seconds(count),
        "m" => TimeDelta::minutes(count),
        "h" => TimeDelta::hours(count),
        "d" => TimeDelta::days(count),
        "w" => TimeDelta::weeks(count),
        other => {
            return Err(format!(
                "unknown unit {other:?}: use s, m, h, d or w, e.g. 30m, 12h, 7d"
            ));
        }
    };
    if span > TimeDelta::days(MAX_SPAN_DAYS) {
        return Err(format!(
            "the activity log retains {RETENTION_DAYS} days and the API rejects \
             windows over {MAX_SPAN_DAYS} days — use {RETENTION_DAYS}d or less"
        ));
    }
    Ok(span)
}

/// Run the `events` command.
pub async fn run(opts: ListOptions<'_>, output: &mut dyn Output) -> Result<(), CliError> {
    run_at(opts, Utc::now(), output).await
}

/// [`run`] with an injected clock, the testable seam — the header
/// and the window bounds derive from `now`.
pub async fn run_at(
    opts: ListOptions<'_>,
    now: DateTime<Utc>,
    output: &mut dyn Output,
) -> Result<(), CliError> {
    let ctx = CommandContext::resolve(opts.repository, opts.token, opts.api_url)?;
    let client = ctx.mergify_client()?;

    let window = Window::last(opts.since.unwrap_or(DEFAULT_SINCE), now)
        // `parse_since` already enforced the cap; anything left is a
        // caller bug surfaced as the typed message rather than a panic.
        .map_err(|e| CliError::InvalidState(e.to_string()))?;

    let scope = match opts.pr_number {
        Some(n) => format!("PR #{n}"),
        None => ctx.repository.clone(),
    };
    output.status(&format!("Fetching events for {scope}…"))?;

    let query = Query {
        pull_request: opts.pr_number,
        event_types: opts.event_types.clone(),
        window,
        limit: Some(opts.limit),
    };
    let log = client::fetch(&client, &ctx.repository, &query).await?;

    if opts.output_json {
        return emit_json(output, &ctx.repository, &opts, &window, &log);
    }

    let theme = Theme::detect();
    let timeline = Timeline {
        scope: &scope,
        window: &window,
        events: &log.events,
        truncated: log.truncated,
        // Repo-wide, the pull request is the only thing telling two
        // `action.queue.checks.change` lines apart. Under `--pr` the
        // header already named it, so the column would be a stutter.
        show_pull_request: opts.pr_number.is_none(),
    };
    output.emit(&(), &mut |w: &mut dyn Write| render(w, &theme, &timeline))?;
    Ok(())
}

/// `--json`: the query echoed back plus the raw events, newest
/// first. Echoing `received_from`/`received_to` keeps the
/// anti-ambiguity contract for machine consumers too — an empty
/// `events` names the window it is empty *over*, and `truncated` says
/// whether the window holds more than `events` shows, so a script
/// reading `size` is never quietly reading a default cap.
fn emit_json(
    output: &mut dyn Output,
    repository: &str,
    opts: &ListOptions<'_>,
    window: &Window,
    log: &client::Log,
) -> Result<(), CliError> {
    let raw: Vec<&Value> = log.events.iter().map(|event| &event.raw).collect();
    output.emit_json_value(&serde_json::json!({
        "repository": repository,
        "pull_request": opts.pr_number,
        "received_from": window.from().to_rfc3339(),
        "received_to": window.to().to_rfc3339(),
        "size": log.events.len(),
        "truncated": log.truncated,
        "events": raw,
    }))?;
    Ok(())
}

/// One rendered result: what the human renderer needs beyond the
/// events themselves.
struct Timeline<'a> {
    /// The repository, or `PR #N` — whatever the header names.
    scope: &'a str,
    window: &'a Window,
    events: &'a [Event],
    /// The limit stopped the fetch with events left in the window.
    truncated: bool,
    /// Name each event's pull request in its own column.
    show_pull_request: bool,
}

/// Whether [`write_header`] already said everything there was to say.
#[derive(PartialEq, Eq)]
enum Header {
    WasEmpty,
    EventsFollow,
}

/// The scope, the count, the window, and every caveat attached to
/// them. Each caveat states a cut this page made and the flag that
/// undoes it: the limit at the old end, the retention behind the
/// window.
fn write_header(
    w: &mut dyn Write,
    theme: &Theme,
    timeline: &Timeline<'_>,
) -> std::io::Result<Header> {
    let &Timeline {
        scope,
        window,
        events,
        truncated,
        ..
    } = timeline;
    let from = format_minute(window.from());
    let to = format_minute(window.to());

    if events.is_empty() {
        // The empty case must name the window: "nothing in the last
        // 24h" and "nothing ever" are different answers, and the
        // second one is not this command's to give.
        writeln!(w, "No events for {scope} between {from} and {to} UTC.")?;
        if window.from() > window.to() - TimeDelta::days(RETENTION_DAYS) {
            writeln!(
                w,
                "{D}Retention is {RETENTION_DAYS} days; try --since {RETENTION_DAYS}d.{R}",
                D = theme.dim,
                R = theme.reset,
            )?;
        }
        return Ok(Header::WasEmpty);
    }

    let count = if truncated {
        format!("newest {n} events", n = events.len())
    } else if events.len() == 1 {
        "1 event".to_string()
    } else {
        format!("{n} events", n = events.len())
    };
    writeln!(
        w,
        "{B}{scope}{R} {D}· {count} · {from} → {to} UTC{R}",
        B = theme.bold,
        R = theme.reset,
        D = theme.dim,
    )?;
    if truncated {
        // The cut is at the *old* end, which — the timeline reading
        // down the page — is the top of the list. Saying so where the
        // cut is, and naming the flag that undoes it, is what keeps a
        // default cap from reading as "that was everything".
        writeln!(
            w,
            "{D}Older events in this window were not fetched — raise --limit.{R}",
            D = theme.dim,
            R = theme.reset,
        )?;
    }
    writeln!(w)?;
    Ok(Header::EventsFollow)
}

fn render(w: &mut dyn Write, theme: &Theme, timeline: &Timeline<'_>) -> std::io::Result<()> {
    let &Timeline {
        window,
        events,
        show_pull_request,
        ..
    } = timeline;
    if write_header(w, theme, timeline)? == Header::WasEmpty {
        return Ok(());
    }

    let type_width = events
        .iter()
        .map(|event| event.event_type().unwrap_or("?").chars().count())
        .max()
        .unwrap_or(0);
    let pr_width = if show_pull_request {
        events
            .iter()
            .filter_map(|event| Some(pr_cell(event)?.chars().count()))
            .max()
            .unwrap_or(0)
    } else {
        0
    };

    // Oldest first: a timeline reads down the page. Date rows appear
    // wherever the calendar date changes — and before the first event
    // when the window spans more than one date, where a bare time
    // would be ambiguous.
    let mut current_date: Option<String> = None;
    let multi_date = window.from().date_naive() != window.to().date_naive();
    for event in events.iter().rev() {
        let stamp = event.received_at_utc();
        if let Some(date) = stamp.map(|ts| ts.format("%Y-%m-%d").to_string()) {
            let changed = current_date.as_ref().is_some_and(|d| *d != date);
            if changed || (current_date.is_none() && multi_date) {
                writeln!(w, "  {D}{date}{R}", D = theme.dim, R = theme.reset)?;
            }
            current_date = Some(date);
        }
        let time = stamp.map_or_else(|| "--:--".to_string(), |ts| ts.format("%H:%M").to_string());
        write!(w, "  {D}{time}{R}", D = theme.dim, R = theme.reset)?;
        if pr_width > 0 {
            match pr_cell(event) {
                Some(pr) => write!(
                    w,
                    "  {N}{pr:<pr_width$}{R}",
                    N = theme.cyan,
                    R = theme.reset,
                )?,
                // A repository-level event (a freeze, a config
                // change) has no pull request: hold the column so the
                // ones that do stay aligned.
                None => write!(w, "  {blank:<pr_width$}", blank = "")?,
            }
        }
        let event_type = event.event_type().unwrap_or("?");
        write!(w, "  {event_type:<type_width$}")?;
        match summary(event) {
            Some(summary) => writeln!(
                w,
                "  {S}{text}{R}",
                S = tone_style(theme, summary.tone),
                text = summary.text,
                R = theme.reset,
            )?,
            None => writeln!(w)?,
        }
    }
    Ok(())
}

/// The event's pull request as it prints, or `None` for the
/// repository-level events that have none.
fn pr_cell(event: &Event) -> Option<String> {
    event.pull_request().map(|number| format!("#{number}"))
}

fn format_minute(ts: DateTime<Utc>) -> String {
    ts.format("%Y-%m-%d %H:%M").to_string()
}

/// How a summary reads. The colour *is* the classification — a
/// timeline where `success` and `failure` are the same grey makes
/// the reader parse every word to find the one that matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    /// A fact with no verdict in it: a queue name, a command author.
    Muted,
    Good,
    /// Worth a look, but not a failure — the merge queue interrupted
    /// the checks, or something is still running.
    Warn,
    Bad,
}

const fn tone_style(theme: &Theme, tone: Tone) -> Style {
    match tone {
        Tone::Muted => theme.dim,
        Tone::Good => theme.green,
        Tone::Warn => theme.yellow,
        Tone::Bad => theme.red,
    }
}

/// One event line's trailing cell: the text and what it means.
struct Summary {
    text: String,
    tone: Tone,
}

impl Summary {
    fn muted(text: String) -> Self {
        Self {
            text,
            tone: Tone::Muted,
        }
    }

    const fn toned(text: String, tone: Tone) -> Self {
        Self { text, tone }
    }
}

/// One best-effort hint per event line, read from the metadata each
/// type is known to carry. Everything here degrades: an unknown type
/// (or a known type missing a field) just renders without a summary —
/// `--json` has the whole payload.
///
/// Which types earn an arm is measured, not guessed. Over 500
/// consecutive events on `Mergifyio/monorepo`, 93% were the two queue
/// state-change types below (`action.queue.change` 233,
/// `action.queue.checks.change` 232) — neither carries a pull request,
/// so without their metadata a default page is a wall of the same two
/// lines. The rest of the arms cover everything else that appeared
/// more than once: `action.label`, `action.request_reviews`,
/// `action.comment`.
fn summary(event: &Event) -> Option<Summary> {
    // A type this function knows about can still arrive without the
    // field it knows about — from an older engine, or from a payload
    // shape that changed. That is a miss, not a reason to print
    // nothing: fall back to what every event has.
    from_metadata(event).or_else(|| {
        outcome_summary(event).or_else(|| event.trigger().map(str::to_owned).map(Summary::muted))
    })
}

/// The per-type arm of [`summary`]; `None` means "nothing specific to
/// say", which the caller turns into the generic fallback.
fn from_metadata(event: &Event) -> Option<Summary> {
    let meta = event.metadata();
    let text = |key: &str| {
        meta.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let flag = |key: &str| meta.get(key).and_then(Value::as_bool).unwrap_or(false);
    let count = |key: &str| meta.get(key).and_then(Value::as_u64);
    // The queue-state events carry no pull request: the queue they
    // moved and the depth they moved it to is all they are.
    let queue_state = |unit: &str, key: &str| {
        let queue = text("queue_name")?;
        let n = count(key)?;
        Some(Summary::muted(format!(
            "{queue} · {n} {unit}{s}",
            s = if n == 1 { "" } else { "s" },
        )))
    };

    match event.event_type().unwrap_or_default() {
        "action.queue.change" => queue_state("PR", "size"),
        "action.queue.checks.change" => queue_state("check", "running_checks"),
        // Which labels, not that labelling happened: the trigger this
        // falls back to otherwise ("Rule: label on unresolved") is the
        // same string on every line the rule ever fired.
        "action.label" => {
            let mut parts = list_of(meta.get("added"), "+");
            parts.extend(list_of(meta.get("removed"), "-"));
            (!parts.is_empty()).then(|| Summary::muted(parts.join(" ")))
        }
        "action.request_reviews" => {
            let mut parts = list_of(meta.get("reviewers"), "@");
            parts.extend(list_of(meta.get("team_reviewers"), "@"));
            (!parts.is_empty()).then(|| Summary::muted(parts.join(" ")))
        }
        // What was posted, first line only — a comment body is
        // multi-line and this is one column of a timeline.
        "action.comment" => text("message").map(|message| Summary::muted(first_line(&message))),
        "action.queue.enter" => text("queue_name").map(Summary::muted),
        "action.queue.checks_start" => {
            let mut parts: Vec<String> = Vec::new();
            parts.extend(text("queue_name"));
            if let Some(draft) = meta
                .get("speculative_check_pull_request")
                .and_then(Value::as_u64)
            {
                parts.push(format!("draft PR #{draft}"));
            }
            (!parts.is_empty()).then(|| Summary::muted(parts.join(" · ")))
        }
        // The abort codes riding on checks_end interrupt the checks
        // while the PR stays queued — naming them here is what keeps
        // a reader from mistaking one for a dequeue. Yellow, not red,
        // carries that same distinction at a glance: red is reserved
        // for the leave below, which is the one that ejected the PR.
        "action.queue.checks_end" if flag("aborted") => {
            text("abort_code").map(|code| Summary::toned(code, Tone::Warn))
        }
        "action.queue.leave" => {
            if flag("merged") {
                Some(Summary::toned("merged".to_string(), Tone::Good))
            } else {
                // Red matches `queue show`, which paints the same
                // dequeue the same way.
                text("dequeue_code").map(|code| Summary::toned(code, Tone::Bad))
            }
        }
        // Who ran it is the fact about a command; its outcome, which
        // the fallback would prefer, is about what the command asked
        // for and shows up on the action events that follow.
        t if t.starts_with("command.") => event.trigger().map(str::to_owned).map(Summary::muted),
        _ => None,
    }
}

/// A metadata array of strings, each prefixed — skipping the empty
/// arrays the API sends for the half of a pair that didn't happen
/// (`{"reviewers": [], "team_reviewers": ["devs"]}`).
fn list_of(value: Option<&Value>, prefix: &str) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|item| format!("{prefix}{item}"))
                .collect()
        })
        .unwrap_or_default()
}

/// The first line, clipped to what a timeline column can hold. Counts
/// characters, not bytes: the messages this renders routinely carry
/// emoji, and a byte slice would panic mid-codepoint.
fn first_line(text: &str) -> String {
    const MAX: usize = 72;
    let line = text.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= MAX {
        return line.to_owned();
    }
    let kept: String = line.chars().take(MAX - 1).collect();
    format!("{kept}…")
}

/// The API's derived outcome, coloured by what it says.
///
/// `neutral` is dropped rather than printed: it is the API's word
/// for "this event has no verdict", and a repo-wide timeline of
/// `action.queue.checks.change  neutral` repeated twenty times is
/// exactly the noise that hid the real events. Falling through to
/// the trigger says something instead.
fn outcome_summary(event: &Event) -> Option<Summary> {
    let outcome = event.outcome()?;
    let tone = match outcome {
        "success" => Tone::Good,
        "failure" | "error" | "cancelled" => Tone::Bad,
        "pending" => Tone::Warn,
        "neutral" => return None,
        // An outcome word from a newer engine still prints, just
        // without a claim about what it means.
        _ => Tone::Muted,
    };
    Some(Summary::toned(outcome.to_owned(), tone))
}

#[cfg(test)]
mod tests {
    use mergify_core::OutputMode;
    use mergify_test_support::Captured;
    use serde_json::json;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;
    use wiremock::matchers::query_param;
    use wiremock::matchers::query_param_is_missing;

    use super::*;

    fn at(iso: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(iso)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn queue_lifecycle() -> Vec<Value> {
        // Newest first, as the API serves them.
        vec![
            json!({
                "id": 5,
                "type": "command.queue",
                "received_at": "2026-07-30T15:12:00Z",
                "trigger": "@jd",
                "pull_request": 1740,
                "metadata": {},
            }),
            json!({
                "id": 4,
                "type": "action.queue.leave",
                "received_at": "2026-07-30T15:04:00Z",
                "trigger": "merge queue internal",
                "pull_request": 1740,
                "metadata": {"merged": false, "dequeue_code": "CHECKS_FAILED"},
            }),
            json!({
                "id": 3,
                "type": "action.queue.checks_end",
                "received_at": "2026-07-30T15:04:00Z",
                "outcome": "failure",
                "pull_request": 1740,
                "metadata": {"aborted": false},
            }),
            json!({
                "id": 2,
                "type": "action.queue.checks_start",
                "received_at": "2026-07-30T14:31:00Z",
                "pull_request": 1740,
                "metadata": {"queue_name": "default", "speculative_check_pull_request": 1801},
            }),
            json!({
                "id": 1,
                "type": "action.queue.enter",
                "received_at": "2026-07-30T14:02:00Z",
                "pull_request": 1740,
                "metadata": {"queue_name": "default"},
            }),
        ]
    }

    async fn arrange(server: &MockServer, events: Vec<Value>) {
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/logs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "size": events.len(),
                "per_page": 100,
                "events": events,
            })))
            .expect(1)
            .mount(server)
            .await;
    }

    /// The knobs the tests vary; everything else is fixed. `limit:
    /// None` is the no-flag invocation — the CLI's own default.
    #[derive(Default)]
    struct Args {
        pr_number: Option<u64>,
        since: Option<TimeDelta>,
        event_types: Vec<String>,
        limit: Option<usize>,
    }

    impl Args {
        fn pr(number: u64) -> Self {
            Self {
                pr_number: Some(number),
                ..Self::default()
            }
        }
    }

    async fn run_list(server: &MockServer, args: Args, output_json: bool) -> Captured {
        let api_url = server.uri();
        let mut cap = if output_json {
            Captured::new(OutputMode::Json)
        } else {
            Captured::human()
        };
        run_at(
            ListOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                pr_number: args.pr_number,
                since: args.since,
                event_types: args.event_types,
                limit: args.limit.unwrap_or(DEFAULT_LIMIT),
                output_json,
            },
            at("2026-07-30T21:00:00Z"),
            &mut cap.output,
        )
        .await
        .unwrap();
        cap
    }

    #[tokio::test]
    async fn human_header_states_scope_count_and_window() {
        let server = MockServer::start().await;
        arrange(&server, queue_lifecycle()).await;

        let cap = run_list(&server, Args::pr(1740), false).await;
        let stdout = cap.stdout();
        // The window is the whole point: an empty-looking day must
        // never read as an empty history.
        assert!(
            stdout.contains("PR #1740 · 5 events · 2026-07-29 21:00 → 2026-07-30 21:00 UTC"),
            "got: {stdout}",
        );
    }

    #[tokio::test]
    async fn human_timeline_reads_oldest_first_with_summaries() {
        let server = MockServer::start().await;
        arrange(&server, queue_lifecycle()).await;

        let cap = run_list(&server, Args::pr(1740), false).await;
        let stdout = cap.stdout();
        let enter = stdout.find("action.queue.enter").unwrap();
        let leave = stdout.find("action.queue.leave").unwrap();
        let requeue = stdout.find("command.queue").unwrap();
        assert!(
            enter < leave && leave < requeue,
            "a timeline reads down the page: {stdout}",
        );
        assert!(stdout.contains("14:02"), "got: {stdout}");
        // Per-type summaries from metadata.
        assert!(stdout.contains("default · draft PR #1801"), "got: {stdout}");
        assert!(stdout.contains("CHECKS_FAILED"), "got: {stdout}");
        assert!(stdout.contains("failure"), "got: {stdout}");
        assert!(stdout.contains("@jd"), "got: {stdout}");
    }

    #[tokio::test]
    async fn human_timeline_marks_date_changes() {
        let server = MockServer::start().await;
        arrange(
            &server,
            vec![
                json!({
                    "id": 2,
                    "type": "action.merge",
                    "received_at": "2026-07-30T09:00:00Z",
                    "outcome": "success",
                }),
                json!({
                    "id": 1,
                    "type": "action.queue.enter",
                    "received_at": "2026-07-29T22:00:00Z",
                    "metadata": {"queue_name": "default"},
                }),
            ],
        )
        .await;

        let cap = run_list(&server, Args::default(), false).await;
        let stdout = cap.stdout();
        // Both dates appear as rows: the window spans two dates, so a
        // bare `22:00` would be ambiguous.
        assert!(stdout.contains("  2026-07-29\n"), "got: {stdout}");
        assert!(stdout.contains("  2026-07-30\n"), "got: {stdout}");
    }

    #[tokio::test]
    async fn human_repo_wide_header_names_the_repository() {
        let server = MockServer::start().await;
        arrange(&server, queue_lifecycle()).await;

        let cap = run_list(&server, Args::default(), false).await;
        let stdout = cap.stdout();
        assert!(stdout.contains("owner/repo · 5 events"), "got: {stdout}");
    }

    #[tokio::test]
    async fn human_empty_names_the_window_and_the_retention() {
        // The bug this command removes: an empty result must read as
        // "nothing in this window", never "nothing ever".
        let server = MockServer::start().await;
        arrange(&server, vec![]).await;

        let cap = run_list(&server, Args::pr(1740), false).await;
        let stdout = cap.stdout();
        assert!(
            stdout.contains(
                "No events for PR #1740 between 2026-07-29 21:00 and 2026-07-30 21:00 UTC."
            ),
            "got: {stdout}",
        );
        assert!(
            stdout.contains("Retention is 90 days; try --since 90d."),
            "got: {stdout}",
        );
    }

    #[tokio::test]
    async fn human_empty_at_full_retention_offers_no_wider_window() {
        // `--since 90d` came back empty: there is nothing wider to
        // suggest, and suggesting it anyway would be noise.
        let server = MockServer::start().await;
        arrange(&server, vec![]).await;

        let cap = run_list(
            &server,
            Args {
                since: Some(TimeDelta::days(90)),
                ..Args::pr(1740)
            },
            false,
        )
        .await;
        let stdout = cap.stdout();
        assert!(stdout.contains("No events for PR #1740"), "got: {stdout}");
        assert!(!stdout.contains("try --since"), "got: {stdout}");
    }

    /// `n` events, newest first, one a minute back from noon — enough
    /// of them to outrun the default cap.
    fn many(n: usize) -> Vec<Value> {
        let newest = at("2026-07-30T12:00:00Z");
        (0..n)
            .map(|i| {
                let received_at = newest - TimeDelta::minutes(i64::try_from(i).unwrap());
                json!({
                    "id": n - i,
                    "type": "action.merge",
                    "received_at": received_at.to_rfc3339(),
                    "pull_request": 1700 + i,
                    "outcome": "success",
                })
            })
            .collect()
    }

    #[tokio::test]
    async fn human_header_says_newest_when_the_limit_bites() {
        // A silent cap would read as "that's everything"; the header
        // must say the list was cut.
        let server = MockServer::start().await;
        arrange(&server, queue_lifecycle()).await;

        let cap = run_list(
            &server,
            Args {
                limit: Some(2),
                ..Args::pr(1740)
            },
            false,
        )
        .await;
        let stdout = cap.stdout();
        assert!(stdout.contains("newest 2 events"), "got: {stdout}");
    }

    #[tokio::test]
    async fn human_truncation_names_the_flag_that_undoes_it() {
        // Saying "newest 100" without saying how to see the rest
        // leaves the reader with a truthful dead end. This is also
        // the no-flag path: one page over, one page printed.
        let server = MockServer::start().await;
        arrange(&server, many(DEFAULT_LIMIT + 5)).await;

        let cap = run_list(&server, Args::default(), false).await;
        let stdout = cap.stdout();
        assert!(
            stdout.contains(&format!("newest {DEFAULT_LIMIT} events")),
            "got: {stdout}",
        );
        assert!(stdout.contains("Older events"), "got: {stdout}");
        assert!(stdout.contains("--limit"), "got: {stdout}");
        assert_eq!(stdout.matches("action.merge").count(), DEFAULT_LIMIT);
    }

    #[tokio::test]
    async fn human_says_nothing_about_truncation_when_the_window_fit() {
        // The window held fewer events than the cap: claiming a cut
        // would be as wrong as hiding one.
        let server = MockServer::start().await;
        arrange(&server, queue_lifecycle()).await;

        let cap = run_list(&server, Args::default(), false).await;
        let stdout = cap.stdout();
        assert!(stdout.contains("owner/repo · 5 events"), "got: {stdout}");
        assert!(!stdout.contains("Older events"), "got: {stdout}");
        assert!(!stdout.contains("newest"), "got: {stdout}");
    }

    #[tokio::test]
    async fn the_default_invocation_asks_for_one_page_and_stops() {
        // The bug: no flags meant no limit, so a busy repository's
        // 24 hours was walked page by page before anything printed.
        // The default is one page — `per_page=100` and `expect(1)`
        // are the assertion, whatever window is left unread.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/logs"))
            .and(query_param("per_page", DEFAULT_LIMIT.to_string().as_str()))
            .and(query_param_is_missing("cursor"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("link", "<https://api.example/logs?cursor=c2>; rel=\"next\"")
                    .set_body_json(json!({
                        "size": DEFAULT_LIMIT,
                        "per_page": DEFAULT_LIMIT,
                        "events": many(DEFAULT_LIMIT),
                    })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let cap = run_list(&server, Args::default(), false).await;
        assert!(
            cap.stdout()
                .contains(&format!("newest {DEFAULT_LIMIT} events")),
            "got: {}",
            cap.stdout(),
        );
    }

    #[tokio::test]
    async fn a_limit_past_one_page_keeps_walking() {
        // `--limit N` is the only way past the default, so it has to
        // reach beyond the single page the default stops at: pages
        // stay capped at the API's 100, and the walk continues until
        // the limit or the window runs out.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/logs"))
            .and(query_param("per_page", "100"))
            .and(query_param_is_missing("cursor"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("link", "<https://api.example/logs?cursor=c2>; rel=\"next\"")
                    .set_body_json(json!({"size": 1, "per_page": 100, "events": [json!({
                        "id": 2,
                        "type": "action.merge",
                        "received_at": "2026-07-30T12:00:00Z",
                        "outcome": "success",
                    })]})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/logs"))
            .and(query_param("cursor", "c2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"size": 1, "per_page": 100, "events": [json!({
                    "id": 1,
                    "type": "action.merge",
                    "received_at": "2026-07-30T11:00:00Z",
                    "outcome": "success",
                })]}),
            ))
            .expect(1)
            .mount(&server)
            .await;

        let cap = run_list(
            &server,
            Args {
                limit: Some(DEFAULT_LIMIT * 2),
                ..Args::default()
            },
            false,
        )
        .await;
        let stdout = cap.stdout();
        // Both pages walked, and nothing claims a cut.
        assert!(stdout.contains("2 events"), "got: {stdout}");
        assert!(!stdout.contains("Older events"), "got: {stdout}");
    }

    #[tokio::test]
    async fn human_repo_wide_lines_name_their_pull_request() {
        // Twenty `action.label` lines are the same line until each
        // says whose it is.
        let server = MockServer::start().await;
        arrange(
            &server,
            vec![
                json!({
                    "id": 2,
                    "type": "action.label",
                    "received_at": "2026-07-30T12:00:00Z",
                    "pull_request": 1801,
                }),
                json!({
                    "id": 1,
                    "type": "action.label",
                    "received_at": "2026-07-30T11:00:00Z",
                    "pull_request": 1740,
                }),
            ],
        )
        .await;

        let cap = run_list(&server, Args::default(), false).await;
        let stdout = cap.stdout();
        assert!(stdout.contains("#1740"), "got: {stdout}");
        assert!(stdout.contains("#1801"), "got: {stdout}");
    }

    #[tokio::test]
    async fn human_pr_scope_leaves_the_pull_request_column_out() {
        // The header already said `PR #1740`; a column repeating it
        // on every line is noise.
        let server = MockServer::start().await;
        arrange(&server, queue_lifecycle()).await;

        let cap = run_list(&server, Args::pr(1740), false).await;
        let stdout = cap.stdout();
        assert!(stdout.contains("PR #1740 · 5 events"), "got: {stdout}");
        let body = stdout.split_once("UTC\n").unwrap().1;
        assert!(!body.contains("#1740"), "got: {stdout}");
    }

    #[tokio::test]
    async fn human_holds_the_column_for_a_repository_level_event() {
        // A freeze belongs to no pull request: the column stays,
        // empty, so the ones that have a PR still line up.
        let server = MockServer::start().await;
        arrange(
            &server,
            vec![
                json!({
                    "id": 2,
                    "type": "action.queue.enter",
                    "received_at": "2026-07-30T12:00:00Z",
                    "pull_request": 1740,
                    "metadata": {"queue_name": "default"},
                }),
                json!({
                    "id": 1,
                    "type": "queue.freeze.create",
                    "received_at": "2026-07-30T11:00:00Z",
                    "metadata": {},
                }),
            ],
        )
        .await;

        let cap = run_list(&server, Args::default(), false).await;
        let stdout = cap.stdout();
        let freeze = stdout
            .lines()
            .find(|line| line.contains("queue.freeze.create"))
            .unwrap();
        let enter = stdout
            .lines()
            .find(|line| line.contains("action.queue.enter"))
            .unwrap();
        assert_eq!(
            freeze.find("queue.freeze.create"),
            enter.find("action.queue.enter"),
            "the type column must start at the same offset: {stdout}",
        );
    }

    #[tokio::test]
    async fn human_survives_an_unknown_event_type() {
        let server = MockServer::start().await;
        arrange(
            &server,
            vec![json!({
                "id": 1,
                "type": "something.from.2027",
                "received_at": "2026-07-30T14:00:00Z",
                "metadata": {"mystery": true},
            })],
        )
        .await;

        let cap = run_list(&server, Args::default(), false).await;
        let stdout = cap.stdout();
        assert!(stdout.contains("something.from.2027"), "got: {stdout}");
        assert!(stdout.contains("14:00"), "got: {stdout}");
    }

    #[tokio::test]
    async fn json_echoes_the_query_and_republishes_raw_events_newest_first() {
        let server = MockServer::start().await;
        let events = queue_lifecycle();
        arrange(&server, events.clone()).await;

        let cap = run_list(&server, Args::pr(1740), true).await;
        let parsed: Value = serde_json::from_str(&cap.stdout()).unwrap();
        assert_eq!(
            parsed,
            json!({
                "repository": "owner/repo",
                "pull_request": 1740,
                "received_from": "2026-07-29T21:00:00+00:00",
                "received_to": "2026-07-30T21:00:00+00:00",
                "size": 5,
                "truncated": false,
                "events": events,
            }),
        );
    }

    #[tokio::test]
    async fn json_says_when_the_default_limit_cut_the_window() {
        // A script reading `size` must be able to tell "that was the
        // window" from "that was the cap".
        let server = MockServer::start().await;
        arrange(&server, many(DEFAULT_LIMIT + 5)).await;

        let cap = run_list(&server, Args::default(), true).await;
        let parsed: Value = serde_json::from_str(&cap.stdout()).unwrap();
        assert_eq!(parsed["truncated"], json!(true));
    }

    #[tokio::test]
    async fn json_keeps_unknown_fields_intact() {
        let server = MockServer::start().await;
        let raw = json!({
            "id": 1,
            "type": "action.queue.leave",
            "received_at": "2026-07-30T14:00:00Z",
            "field_from_2027": {"nested": [1, 2, 3]},
        });
        arrange(&server, vec![raw.clone()]).await;

        let cap = run_list(&server, Args::default(), true).await;
        let parsed: Value = serde_json::from_str(&cap.stdout()).unwrap();
        assert_eq!(parsed["events"][0], raw);
    }

    #[tokio::test]
    async fn json_empty_still_names_the_window() {
        let server = MockServer::start().await;
        arrange(&server, vec![]).await;

        let cap = run_list(&server, Args::default(), true).await;
        let parsed: Value = serde_json::from_str(&cap.stdout()).unwrap();
        assert_eq!(parsed["size"], json!(0));
        assert_eq!(parsed["events"], json!([]));
        assert_eq!(parsed["received_from"], json!("2026-07-29T21:00:00+00:00"));
        assert_eq!(parsed["pull_request"], json!(null));
    }

    /// `render` with colors forced on. `Theme::detect` reports
    /// disabled until the CLI entry point records a `--color` choice,
    /// which no test does, so the styled branch needs driving
    /// explicitly — and what it emits is the whole point of this
    /// change.
    fn render_in_color(events: Vec<Value>) -> String {
        let events: Vec<Event> = events.into_iter().map(Event::from_raw).collect();
        let window = Window::last(DEFAULT_SINCE, at("2026-07-30T21:00:00Z")).unwrap();
        let mut out: Vec<u8> = Vec::new();
        render(
            &mut out,
            &Theme::new(true),
            &Timeline {
                scope: "owner/repo",
                window: &window,
                events: &events,
                truncated: false,
                show_pull_request: true,
            },
        )
        .unwrap();
        String::from_utf8(out).unwrap()
    }

    /// The SGR sequence `style` opens with, as it lands in the output.
    fn code(style: Style) -> String {
        format!("{style}")
    }

    #[test]
    fn success_and_failure_do_not_look_alike() {
        // The complaint: everything grey, so the one word that
        // matters reads like all the others.
        let theme = Theme::new(true);
        let stdout = render_in_color(vec![
            json!({
                "id": 2,
                "type": "action.merge",
                "received_at": "2026-07-30T12:00:00Z",
                "pull_request": 1740,
                "outcome": "success",
            }),
            json!({
                "id": 1,
                "type": "action.queue.checks_end",
                "received_at": "2026-07-30T11:00:00Z",
                "pull_request": 1740,
                "outcome": "failure",
                "metadata": {"aborted": false},
            }),
        ]);
        assert!(
            stdout.contains(&format!("{}failure", code(theme.red))),
            "got: {stdout:?}",
        );
        assert!(
            stdout.contains(&format!("{}success", code(theme.green))),
            "got: {stdout:?}",
        );
    }

    #[test]
    fn the_queue_verdicts_carry_their_own_colors() {
        // The distinction the timeline exists to make readable: an
        // abort left the PR queued (yellow), a dequeue ejected it
        // (red, as `queue show` paints it), a merge is the good end.
        let theme = Theme::new(true);
        let stdout = render_in_color(vec![
            json!({
                "id": 3,
                "type": "action.queue.leave",
                "received_at": "2026-07-30T13:00:00Z",
                "pull_request": 1740,
                "metadata": {"merged": true},
            }),
            json!({
                "id": 2,
                "type": "action.queue.leave",
                "received_at": "2026-07-30T12:00:00Z",
                "pull_request": 1741,
                "metadata": {"merged": false, "dequeue_code": "CHECKS_FAILED"},
            }),
            json!({
                "id": 1,
                "type": "action.queue.checks_end",
                "received_at": "2026-07-30T11:00:00Z",
                "pull_request": 1742,
                "metadata": {"aborted": true, "abort_code": "PR_AHEAD_DEQUEUED"},
            }),
        ]);
        assert!(
            stdout.contains(&format!("{}PR_AHEAD_DEQUEUED", code(theme.yellow))),
            "got: {stdout:?}",
        );
        assert!(
            stdout.contains(&format!("{}CHECKS_FAILED", code(theme.red))),
            "got: {stdout:?}",
        );
        assert!(
            stdout.contains(&format!("{}merged", code(theme.green))),
            "got: {stdout:?}",
        );
    }

    #[test]
    fn the_pull_request_column_reads_as_an_identifier() {
        // Cyan `#N` is what `queue status` already uses for a pull
        // request; the timeline must not invent a second convention.
        let theme = Theme::new(true);
        let stdout = render_in_color(vec![json!({
            "id": 1,
            "type": "action.queue.enter",
            "received_at": "2026-07-30T12:00:00Z",
            "pull_request": 1740,
            "metadata": {"queue_name": "default"},
        })]);
        assert!(
            stdout.contains(&format!("{}#1740", code(theme.cyan))),
            "got: {stdout:?}",
        );
    }

    #[test]
    fn a_neutral_outcome_yields_to_the_trigger() {
        // `neutral` is the API saying "no verdict here". Printing it
        // is what turned the repo-wide view into a column of the same
        // word; the trigger at least says who did it.
        let event = Event::from_raw(json!({
            "type": "action.label",
            "outcome": "neutral",
            "trigger": "Rule: automatic label",
        }));
        let cell = summary(&event).unwrap();
        assert_eq!(cell.text, "Rule: automatic label");
        assert_eq!(cell.tone, Tone::Muted);

        // Nothing to fall back to: no summary beats a bare `neutral`.
        let bare = Event::from_raw(json!({"type": "action.label", "outcome": "neutral"}));
        assert!(summary(&bare).is_none());
    }

    #[test]
    fn the_queue_state_events_report_the_queue_they_moved() {
        // 93% of a busy repository's default page: no pull request,
        // `neutral` outcome, and the same trigger on every one. The
        // depth is the only thing that changes between them, so it is
        // the only thing worth printing.
        let change = Event::from_raw(json!({
            "type": "action.queue.change",
            "outcome": "neutral",
            "trigger": "merge queue internal",
            "metadata": {"queue_name": "default", "size": 3},
        }));
        assert_eq!(summary(&change).unwrap().text, "default · 3 PRs");

        let checks = Event::from_raw(json!({
            "type": "action.queue.checks.change",
            "outcome": "neutral",
            "trigger": "merge queue internal",
            "metadata": {"queue_name": "default", "running_checks": 1},
        }));
        assert_eq!(summary(&checks).unwrap().text, "default · 1 check");
    }

    #[test]
    fn a_label_event_names_the_labels() {
        // "Rule: label on unresolved" is the same string on every
        // line the rule ever fired; the label is what differs.
        let event = Event::from_raw(json!({
            "type": "action.label",
            "outcome": "neutral",
            "trigger": "Rule: label on unresolved",
            "metadata": {"added": ["conflict"], "removed": ["review threads unresolved"]},
        }));
        assert_eq!(
            summary(&event).unwrap().text,
            "+conflict -review threads unresolved",
        );
    }

    #[test]
    fn a_review_request_names_the_reviewers() {
        // The API sends the empty half of the pair, and an empty list
        // must not print as a stray separator.
        let event = Event::from_raw(json!({
            "type": "action.request_reviews",
            "outcome": "neutral",
            "metadata": {"reviewers": [], "team_reviewers": ["devs"]},
        }));
        assert_eq!(summary(&event).unwrap().text, "@devs");
    }

    #[test]
    fn a_comment_event_quotes_its_first_line() {
        let event = Event::from_raw(json!({
            "type": "action.comment",
            "outcome": "neutral",
            "metadata": {"message": "@kozlek this pull request is now in conflict 😩\n\nsecond line"},
        }));
        assert_eq!(
            summary(&event).unwrap().text,
            "@kozlek this pull request is now in conflict 😩",
        );
    }

    #[test]
    fn a_long_comment_is_clipped_on_a_character_boundary() {
        // Real comment bodies carry emoji: clipping by bytes would
        // panic mid-codepoint, and a timeline column has to end
        // somewhere.
        let event = Event::from_raw(json!({
            "type": "action.comment",
            "metadata": {"message": "😩".repeat(200)},
        }));
        let text = summary(&event).unwrap().text;
        assert_eq!(text.chars().count(), 72);
        assert!(text.ends_with('…'), "got: {text}");
    }

    #[test]
    fn a_known_type_missing_its_metadata_still_falls_back() {
        // An older engine, or a payload shape that moved: a taught
        // type with nothing to read must degrade to what every event
        // has, not print an empty column.
        let event = Event::from_raw(json!({
            "type": "action.label",
            "outcome": "neutral",
            "trigger": "Rule: automatic label",
        }));
        assert_eq!(summary(&event).unwrap().text, "Rule: automatic label");
    }

    #[test]
    fn an_unknown_outcome_still_prints() {
        // A verdict word from a newer engine renders, uncolored —
        // this CLI does not get to claim it knows what it means.
        let event = Event::from_raw(json!({"type": "action.new", "outcome": "quarantined"}));
        let cell = summary(&event).unwrap();
        assert_eq!(cell.text, "quarantined");
        assert_eq!(cell.tone, Tone::Muted);
    }

    #[test]
    fn parse_since_reads_the_documented_units() {
        assert_eq!(parse_since("45s").unwrap(), TimeDelta::seconds(45));
        assert_eq!(parse_since("30m").unwrap(), TimeDelta::minutes(30));
        assert_eq!(parse_since("12h").unwrap(), TimeDelta::hours(12));
        assert_eq!(parse_since("7d").unwrap(), TimeDelta::days(7));
        assert_eq!(parse_since("2w").unwrap(), TimeDelta::weeks(2));
    }

    #[test]
    fn parse_since_rejects_garbage_with_the_format_in_the_message() {
        for bad in ["", "7", "d", "-7d", "0d", "7x", "1.5h"] {
            let err = parse_since(bad).unwrap_err();
            assert!(
                err.contains("7d") || err.contains("30m"),
                "the error must show the expected shape, got: {err}",
            );
        }
    }

    #[test]
    fn parse_since_rejects_a_span_past_the_cap_and_names_the_fix() {
        let err = parse_since("94d").unwrap_err();
        assert!(err.contains("90d"), "got: {err}");
        assert!(parse_since("93d").is_ok());
        let err = parse_since("14w").unwrap_err();
        assert!(err.contains("90d"), "got: {err}");
    }
}
