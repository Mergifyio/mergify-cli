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
//! Two output modes:
//!
//! - Human (default): a header naming the scope, the event count and
//!   the exact UTC window, then one line per event, **oldest first**
//!   (a timeline reads down the page), with a dim date row wherever
//!   the calendar date changes.
//! - `--json`: a single document with the query echoed
//!   (`repository`, `pull_request`, `received_from`, `received_to`),
//!   `size`, and `events` — the raw API events **newest first**,
//!   unknown fields intact, matching the client's ordering guarantee.

use std::io::Write;

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
    /// Stop after the newest N events instead of fetching the whole
    /// window. The header says so when it takes effect.
    pub limit: Option<usize>,
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
        limit: opts.limit,
    };
    let events = client::fetch(&client, &ctx.repository, &query).await?;

    if opts.output_json {
        return emit_json(output, &ctx.repository, &opts, &window, &events);
    }

    let theme = Theme::detect();
    let truncated = opts.limit.is_some_and(|limit| events.len() == limit);
    output.emit(&(), &mut |w: &mut dyn Write| {
        render(w, &theme, &scope, &window, &events, truncated)
    })?;
    Ok(())
}

/// `--json`: the query echoed back plus the raw events, newest
/// first. Echoing `received_from`/`received_to` keeps the
/// anti-ambiguity contract for machine consumers too — an empty
/// `events` names the window it is empty *over*.
fn emit_json(
    output: &mut dyn Output,
    repository: &str,
    opts: &ListOptions<'_>,
    window: &Window,
    events: &[Event],
) -> Result<(), CliError> {
    let raw: Vec<&Value> = events.iter().map(|event| &event.raw).collect();
    output.emit_json_value(&serde_json::json!({
        "repository": repository,
        "pull_request": opts.pr_number,
        "received_from": window.from().to_rfc3339(),
        "received_to": window.to().to_rfc3339(),
        "size": events.len(),
        "events": raw,
    }))?;
    Ok(())
}

fn render(
    w: &mut dyn Write,
    theme: &Theme,
    scope: &str,
    window: &Window,
    events: &[Event],
    truncated: bool,
) -> std::io::Result<()> {
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
        return Ok(());
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
    writeln!(w)?;

    let type_width = events
        .iter()
        .map(|event| event.event_type().unwrap_or("?").chars().count())
        .max()
        .unwrap_or(0);

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
        let event_type = event.event_type().unwrap_or("?");
        write!(
            w,
            "  {D}{time}{R}  {event_type:<type_width$}",
            D = theme.dim,
            R = theme.reset,
        )?;
        match summary(event) {
            Some(summary) => writeln!(w, "  {D}{summary}{R}", D = theme.dim, R = theme.reset)?,
            None => writeln!(w)?,
        }
    }
    Ok(())
}

fn format_minute(ts: DateTime<Utc>) -> String {
    ts.format("%Y-%m-%d %H:%M").to_string()
}

/// One best-effort hint per event line, read from the metadata
/// fields the queue family is known to carry. Everything here
/// degrades: an unknown type (or a known type missing a field) just
/// renders without a summary — `--json` has the whole payload.
fn summary(event: &Event) -> Option<String> {
    let meta = event.metadata();
    let text = |key: &str| {
        meta.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let flag = |key: &str| meta.get(key).and_then(Value::as_bool).unwrap_or(false);

    match event.event_type().unwrap_or_default() {
        "action.queue.enter" => text("queue_name"),
        "action.queue.checks_start" => {
            let mut parts: Vec<String> = Vec::new();
            parts.extend(text("queue_name"));
            if let Some(draft) = meta
                .get("speculative_check_pull_request")
                .and_then(Value::as_u64)
            {
                parts.push(format!("draft PR #{draft}"));
            }
            (!parts.is_empty()).then(|| parts.join(" · "))
        }
        // The abort codes riding on checks_end interrupt the checks
        // while the PR stays queued — naming them here is what keeps
        // a reader from mistaking one for a dequeue.
        "action.queue.checks_end" if flag("aborted") => text("abort_code"),
        "action.queue.leave" => {
            if flag("merged") {
                Some("merged".to_string())
            } else {
                text("dequeue_code")
            }
        }
        t if t.starts_with("command.") => event.trigger().map(str::to_owned),
        _ => event
            .outcome()
            .map(str::to_owned)
            .or_else(|| event.trigger().map(str::to_owned)),
    }
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

    async fn run_list(
        server: &MockServer,
        opts_for: impl FnOnce(&str) -> (Option<u64>, Option<TimeDelta>, Vec<String>, Option<usize>),
        output_json: bool,
    ) -> Captured {
        let api_url = server.uri();
        let (pr_number, since, event_types, limit) = opts_for(&api_url);
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
                pr_number,
                since,
                event_types,
                limit,
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

        let cap = run_list(&server, |_| (Some(1740), None, vec![], None), false).await;
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

        let cap = run_list(&server, |_| (Some(1740), None, vec![], None), false).await;
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

        let cap = run_list(&server, |_| (None, None, vec![], None), false).await;
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

        let cap = run_list(&server, |_| (None, None, vec![], None), false).await;
        let stdout = cap.stdout();
        assert!(stdout.contains("owner/repo · 5 events"), "got: {stdout}");
    }

    #[tokio::test]
    async fn human_empty_names_the_window_and_the_retention() {
        // The bug this command removes: an empty result must read as
        // "nothing in this window", never "nothing ever".
        let server = MockServer::start().await;
        arrange(&server, vec![]).await;

        let cap = run_list(&server, |_| (Some(1740), None, vec![], None), false).await;
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
            |_| (Some(1740), Some(TimeDelta::days(90)), vec![], None),
            false,
        )
        .await;
        let stdout = cap.stdout();
        assert!(stdout.contains("No events for PR #1740"), "got: {stdout}");
        assert!(!stdout.contains("try --since"), "got: {stdout}");
    }

    #[tokio::test]
    async fn human_header_says_newest_when_the_limit_bites() {
        // A silent cap would read as "that's everything"; the header
        // must say the list was cut.
        let server = MockServer::start().await;
        let events: Vec<Value> = queue_lifecycle().into_iter().take(2).collect();
        arrange(&server, events).await;

        let cap = run_list(&server, |_| (Some(1740), None, vec![], Some(2)), false).await;
        let stdout = cap.stdout();
        assert!(stdout.contains("newest 2 events"), "got: {stdout}");
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

        let cap = run_list(&server, |_| (None, None, vec![], None), false).await;
        let stdout = cap.stdout();
        assert!(stdout.contains("something.from.2027"), "got: {stdout}");
        assert!(stdout.contains("14:00"), "got: {stdout}");
    }

    #[tokio::test]
    async fn json_echoes_the_query_and_republishes_raw_events_newest_first() {
        let server = MockServer::start().await;
        let events = queue_lifecycle();
        arrange(&server, events.clone()).await;

        let cap = run_list(&server, |_| (Some(1740), None, vec![], None), true).await;
        let parsed: Value = serde_json::from_str(&cap.stdout()).unwrap();
        assert_eq!(
            parsed,
            json!({
                "repository": "owner/repo",
                "pull_request": 1740,
                "received_from": "2026-07-29T21:00:00+00:00",
                "received_to": "2026-07-30T21:00:00+00:00",
                "size": 5,
                "events": events,
            }),
        );
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

        let cap = run_list(&server, |_| (None, None, vec![], None), true).await;
        let parsed: Value = serde_json::from_str(&cap.stdout()).unwrap();
        assert_eq!(parsed["events"][0], raw);
    }

    #[tokio::test]
    async fn json_empty_still_names_the_window() {
        let server = MockServer::start().await;
        arrange(&server, vec![]).await;

        let cap = run_list(&server, |_| (None, None, vec![], None), true).await;
        let parsed: Value = serde_json::from_str(&cap.stdout()).unwrap();
        assert_eq!(parsed["size"], json!(0));
        assert_eq!(parsed["events"], json!([]));
        assert_eq!(parsed["received_from"], json!("2026-07-29T21:00:00+00:00"));
        assert_eq!(parsed["pull_request"], json!(null));
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
