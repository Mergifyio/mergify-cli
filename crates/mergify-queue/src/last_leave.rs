//! Diagnosis fallback for a pull request that is **not** currently in
//! the merge queue.
//!
//! `GET /v1/repos/<repo>/merge-queue/pull/<n>` answers only for a PR
//! that is queued *right now*; it 404s for everything else. That 404
//! is overloaded — it covers "dequeued ten minutes ago on failing
//! CI", "nobody ever queued it", and "that PR number doesn't exist" —
//! so `queue show` falls back to the activity log to tell them apart:
//!
//! ```text
//! GET /v1/repos/<repo>/logs
//!     ?pull_request=<n>
//!     &event_type=action.queue.leave
//!     &received_from=<now - 90d>
//!     &per_page=1
//! ```
//!
//! Four things about that request are load-bearing:
//!
//! - **The window.** `/logs` defaults `received_from` to
//!   `received_to - 1 day`. Without an explicit range a PR dequeued
//!   last week comes back as `size: 0`, which reads exactly like
//!   never-queued — the failure mode this module exists to remove. The
//!   API rejects a span over 93 days (422), and event retention is 90,
//!   so [`LOOKBACK_DAYS`] asks for the whole retained history and no
//!   more.
//! - **The ordering.** Events come back newest-first, so `events[0]`
//!   with `per_page=1` is the PR's *last* transition out of the queue.
//!   A PR that conflicted, was requeued and then merged reports the
//!   merge, not the conflict.
//! - **The event type.** `action.queue.leave` is emitted only when the
//!   PR actually leaves. The sibling `abort_code` values that ride on
//!   `action.queue.checks_end` (`PR_AHEAD_DEQUEUED`,
//!   `MERGE_QUEUE_RESET`, `CHECKS_RETRIED`, …) interrupt the *checks*
//!   while the PR stays queued; filtering on the event type is what
//!   keeps them out of this diagnosis. Reporting one as a dequeue
//!   would tell an agent to requeue a PR that never left.
//! - **`metadata.merged`.** A merge is also an `action.queue.leave`.
//!   It is the boolean, not the `dequeue_code`, that separates "left
//!   by merging" from "was dequeued".
//!
//! Everything user-facing (the sentence, its per-reason hint, the
//! failing check names and URLs) is rendered from what the API
//! returns. The CLI keeps no local copy of the ~32 `dequeue_code`
//! strings: a stale copy would drift, and an unknown code must render
//! rather than crash.

use std::io::Write;

use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;
use mergify_core::CliError;
use mergify_core::http::Client;
use mergify_tui::Theme;
use mergify_tui::relative_time;
use serde::Deserialize;

/// How far back to look for the pull request's last queue exit.
/// Bounded by the API on both sides: `/logs` retains 90 days of
/// events and rejects a `received_from`/`received_to` span over 93
/// days with a 422.
const LOOKBACK_DAYS: i64 = 90;

const LEAVE_EVENT_TYPE: &str = "action.queue.leave";

/// How many lines of the API's `reason` prose to print without
/// `--verbose`. A `PR_DEQUEUED` reason embeds the full unmet
/// condition tree and runs to 60+ lines on a real repository, which
/// buries the headline and the failing checks under it. The first
/// lines carry the sentence and its hint; the tree that follows is
/// what `-v` is for — the same compact/verbose split the in-queue
/// conditions section already uses.
const REASON_COMPACT_LINES: usize = 12;

/// The pull request's last `action.queue.leave`, both as the raw API
/// event (what `--json` republishes, so unknown fields survive) and
/// as the decoded view the human renderer reads.
pub struct LastLeave {
    pub raw: serde_json::Value,
    pub event: LeaveEvent,
}

impl LastLeave {
    /// Whether the PR *left the queue without merging* — the answer
    /// `queued: false` alone could never give.
    #[must_use]
    pub const fn dequeued(&self) -> bool {
        !self.event.metadata.merged
    }

    /// The PR head this diagnosis describes, for a caller that needs
    /// to check it still is the PR's head — see
    /// [`LeaveMetadata::pull_request_head_sha`].
    #[must_use]
    pub fn head_sha(&self) -> Option<&str> {
        non_empty(self.event.metadata.pull_request_head_sha.as_deref())
    }
}

/// Decoded `action.queue.leave` event. Every field is optional: a
/// diagnosis rendered from a payload missing one field is worth more
/// than a hard error, and new API fields must never break the
/// fallback.
#[derive(Deserialize, Default)]
pub struct LeaveEvent {
    #[serde(default)]
    pub received_at: Option<String>,
    #[serde(default)]
    pub trigger: Option<String>,
    #[serde(default)]
    pub metadata: LeaveMetadata,
}

#[derive(Deserialize, Default)]
pub struct LeaveMetadata {
    #[serde(default)]
    pub merged: bool,
    #[serde(default)]
    pub dequeue_code: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub queue_name: Option<String>,
    #[serde(default)]
    pub queued_at: Option<String>,
    /// The PR head the queue was testing when it left. Load-bearing
    /// for anyone acting on this diagnosis: the `unsuccessful_checks`
    /// below belong to *this* commit, and a push since then (the very
    /// thing `PULL_REQUEST_UPDATED` and `DRAFT_PULL_REQUEST_CHANGED`
    /// report) leaves them describing a commit the PR no longer has.
    /// Comparing it against the current head is what tells a live
    /// failure apart from one already superseded.
    #[serde(default)]
    pub pull_request_head_sha: Option<String>,
    #[serde(default)]
    pub unsuccessful_checks: Vec<LeaveCheck>,
}

#[derive(Deserialize, Default)]
pub struct LeaveCheck {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub state: Option<String>,
    /// The check's own page (a workflow-job URL for GitHub Actions).
    /// `queue show`'s in-queue checks section has never carried a URL,
    /// which is why "where is the failure?" needed a second tool.
    #[serde(default)]
    pub details_url: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

impl LeaveCheck {
    /// The best link to the check. `details_url` is the check run's
    /// own page; `url` is the fallback the API sends when the
    /// integration provides no details link.
    fn link(&self) -> Option<&str> {
        self.details_url
            .as_deref()
            .or(self.url.as_deref())
            .filter(|s| !s.is_empty())
    }
}

#[derive(Deserialize)]
struct EventsResponse {
    #[serde(default)]
    events: Vec<serde_json::Value>,
}

/// Fetch the pull request's last exit from the merge queue, or `None`
/// when the activity log holds no `action.queue.leave` for it in the
/// retained window (the genuine never-queued case, and the case of a
/// PR number that never existed).
///
/// # Errors
///
/// Propagates the API failure. Callers treat that as "could not
/// determine", not as a command failure: `queue show` must keep
/// answering (and exiting 0) for a token that cannot read the
/// repository's activity log.
pub async fn fetch(
    client: &Client,
    repository: &str,
    pr_number: u64,
    now: DateTime<Utc>,
) -> Result<Option<LastLeave>, CliError> {
    let path = format!("/v1/repos/{repository}/logs");
    let received_from = (now - TimeDelta::days(LOOKBACK_DAYS)).to_rfc3339();
    let pr = pr_number.to_string();

    let response: EventsResponse = client
        .get_with_query(
            &path,
            &[
                ("pull_request", pr.as_str()),
                ("event_type", LEAVE_EVENT_TYPE),
                ("received_from", received_from.as_str()),
                // Newest-first ordering makes the single returned
                // event the PR's latest exit.
                ("per_page", "1"),
            ],
        )
        .await?;

    let Some(raw) = response.events.into_iter().next() else {
        return Ok(None);
    };
    let event: LeaveEvent = serde_json::from_value(raw.clone())
        .map_err(|e| CliError::wrap("decode queue leave event", e))?;
    Ok(Some(LastLeave { raw, event }))
}

/// Render the diagnosis for a PR that left the merge queue: a
/// headline naming *what* happened and when, an aligned facts block,
/// the API's own reason prose, the failing checks with their URLs,
/// and the next step.
pub fn render(
    w: &mut dyn Write,
    theme: &Theme,
    leave: &LastLeave,
    pr_number: u64,
    now: DateTime<Utc>,
    verbose: bool,
) -> std::io::Result<()> {
    let meta = &leave.event.metadata;
    let when = leave
        .event
        .received_at
        .as_deref()
        .map(|ts| relative_time(ts, now, false))
        .filter(|rel| !rel.is_empty());

    let headline = if meta.merged {
        "was merged by the merge queue"
    } else {
        "was dequeued"
    };
    let style = if meta.merged { theme.green } else { theme.red };
    write!(
        w,
        "{S}PR #{pr_number} {headline}{R}",
        S = style,
        R = theme.reset,
    )?;
    match &when {
        Some(rel) => writeln!(w, " {D}{rel}{R}", D = theme.dim, R = theme.reset)?,
        None => writeln!(w)?,
    }
    writeln!(w)?;

    // Only a dequeue carries a meaningful code — a merge's code is
    // always the tautological PR_MERGED.
    if !meta.merged
        && let Some(code) = non_empty(meta.dequeue_code.as_deref())
    {
        writeln!(
            w,
            "  Dequeue code:  {B}{code}{R}",
            B = theme.bold,
            R = theme.reset
        )?;
    }
    if let Some(queue) = non_empty(meta.queue_name.as_deref()) {
        writeln!(w, "  Queue:         {queue}")?;
    }
    if let Some(rel) = meta
        .queued_at
        .as_deref()
        .map(|ts| relative_time(ts, now, false))
        .filter(|rel| !rel.is_empty())
    {
        writeln!(w, "  Queued at:     {rel}")?;
    }
    if let Some(trigger) = non_empty(leave.event.trigger.as_deref()) {
        writeln!(w, "  Trigger:       {trigger}")?;
    }
    // Printed for a merge too: it names the commit that landed. On a
    // dequeue it is the caveat on everything below — the reason and
    // the check URLs describe this commit, not necessarily the one the
    // PR carries now.
    if let Some(sha) = leave.head_sha() {
        writeln!(w, "  Head SHA:      {sha}", sha = short_sha(sha))?;
    }

    if let Some(reason) = non_empty(meta.reason.as_deref()) {
        writeln!(w)?;
        print_reason(w, theme, reason, verbose)?;
    }

    if !meta.unsuccessful_checks.is_empty() {
        writeln!(w)?;
        print_failing_checks(w, theme, &meta.unsuccessful_checks)?;
    }

    if !meta.merged {
        writeln!(w)?;
        writeln!(
            w,
            "  {D}Fix the cause above, then comment `@mergifyio queue` on the pull request.{R}",
            D = theme.dim,
            R = theme.reset,
        )?;
    }
    Ok(())
}

/// Print the API's own explanation. The engine owns this prose and
/// the per-reason hint embedded in it; it arrives as multi-line
/// markdown, so indent every line rather than squeezing it into the
/// aligned facts block, and cap it at [`REASON_COMPACT_LINES`] unless
/// `verbose`.
fn print_reason(
    w: &mut dyn Write,
    theme: &Theme,
    reason: &str,
    verbose: bool,
) -> std::io::Result<()> {
    let lines: Vec<&str> = reason.lines().collect();
    let shown = if verbose {
        lines.len()
    } else {
        lines.len().min(REASON_COMPACT_LINES)
    };
    for line in &lines[..shown] {
        if line.is_empty() {
            writeln!(w)?;
        } else {
            writeln!(w, "  {line}")?;
        }
    }
    let hidden = lines.len() - shown;
    if hidden > 0 {
        writeln!(
            w,
            "  {D}… {hidden} more lines (-v for the full reason){R}",
            D = theme.dim,
            R = theme.reset,
        )?;
    }
    Ok(())
}

/// Print the checks that were not green when the PR left, each with
/// its URL. `queue show`'s in-queue checks section renders names and
/// states but no links, which is why "where is the failure?" used to
/// need a second tool.
fn print_failing_checks(
    w: &mut dyn Write,
    theme: &Theme,
    checks: &[LeaveCheck],
) -> std::io::Result<()> {
    writeln!(w, "  Failing checks:")?;
    for check in checks {
        write!(
            w,
            "    {S}✗{R} {name}",
            S = theme.red,
            R = theme.reset,
            name = check.name,
        )?;
        match non_empty(check.state.as_deref()) {
            Some(state) => writeln!(w, "  {D}{state}{R}", D = theme.dim, R = theme.reset)?,
            None => writeln!(w)?,
        }
        if let Some(link) = check.link() {
            writeln!(w, "      {D}{link}{R}", D = theme.dim, R = theme.reset)?;
        }
    }
    Ok(())
}

/// Render the "we looked and found nothing" case. Distinct from the
/// bare notice line because "no queue activity in the retained
/// window" is an answer, whereas silence is not.
pub fn render_no_activity(w: &mut dyn Write, theme: &Theme) -> std::io::Result<()> {
    writeln!(
        w,
        "  {D}No merge-queue activity in the last {LOOKBACK_DAYS} days.{R}",
        D = theme.dim,
        R = theme.reset,
    )
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|s| !s.is_empty())
}

/// Abbreviate a commit SHA for display, the way git and GitHub do.
/// `--json` keeps the full value — a caller comparing it against the
/// PR head needs the whole thing.
fn short_sha(sha: &str) -> &str {
    sha.char_indices()
        .nth(7)
        .map_or(sha, |(byte_index, _)| &sha[..byte_index])
}

#[cfg(test)]
mod tests {
    use mergify_core::http::ApiFlavor;
    use serde_json::json;
    use url::Url;
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

    fn client(server: &MockServer) -> Client {
        Client::new(Url::parse(&server.uri()).unwrap(), "t", ApiFlavor::Mergify).unwrap()
    }

    /// Shape copied from a real `Mergifyio/mergify-cli` event.
    fn checks_failed_event() -> serde_json::Value {
        json!({
            "id": 77_235_715,
            "received_at": "2026-07-20T23:25:11.263987Z",
            "trigger": "merge queue internal",
            "repository": "owner/repo",
            "pull_request": 1700,
            "base_ref": "main",
            "outcome": "failure",
            "type": "action.queue.leave",
            "metadata": {
                "reason": "The merge conditions cannot be satisfied due to failing checks\n\n- `ci-gate`",
                "merged": false,
                "queue_name": "default",
                "branch": "main",
                "queued_at": "2026-07-20T23:11:42.944734Z",
                "pull_request_head_sha": "31b4a485b8ce6f2c1d0e9a7b4c5d6e7f80910111",
                "dequeue_code": "CHECKS_FAILED",
                "checks": [],
                "unsuccessful_checks": [{
                    "name": "ci-gate",
                    "description": "",
                    "url": "https://github.com/owner/repo/actions/runs/1/job/2",
                    "details_url": "https://github.com/owner/repo/actions/runs/1/job/2",
                    "state": "failure",
                    "avatar_url": null,
                }],
            },
        })
    }

    async fn arrange(server: &MockServer, events: Vec<serde_json::Value>) {
        let size = events.len();
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/logs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "size": size,
                "per_page": 1,
                "events": events,
            })))
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn fetch_sends_the_queue_leave_query_with_a_90_day_window() {
        // Every one of these four parameters is a documented trap:
        // the default 1-day window hides older dequeues, the event
        // type keeps `checks_end` abort codes (which do NOT dequeue)
        // out, newest-first + per_page=1 selects the latest exit.
        let server = MockServer::start().await;
        arrange(&server, vec![checks_failed_event()]).await;

        let now = at("2026-07-30T00:00:00Z");
        let got = fetch(&client(&server), "owner/repo", 1700, now)
            .await
            .unwrap();
        assert!(got.is_some());

        let received = server.received_requests().await.unwrap();
        let query: Vec<(String, String)> = received[0]
            .url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert!(
            query.contains(&("pull_request".into(), "1700".into())),
            "got: {query:?}",
        );
        assert!(
            query.contains(&("event_type".into(), "action.queue.leave".into())),
            "got: {query:?}",
        );
        assert!(
            query.contains(&("per_page".into(), "1".into())),
            "got: {query:?}",
        );
        let from = query
            .iter()
            .find(|(k, _)| k == "received_from")
            .map(|(_, v)| v.clone())
            .expect("received_from must be explicit, not left to the 1-day default");
        assert_eq!(at(&from), at("2026-05-01T00:00:00Z"));
    }

    #[tokio::test]
    async fn fetch_returns_none_when_the_log_holds_no_leave_event() {
        let server = MockServer::start().await;
        arrange(&server, vec![]).await;

        let got = fetch(
            &client(&server),
            "owner/repo",
            999,
            at("2026-07-30T00:00:00Z"),
        )
        .await
        .unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn fetch_tolerates_an_event_missing_every_optional_field() {
        // A payload the CLI has never seen must still produce a
        // diagnosis rather than a decode error.
        let server = MockServer::start().await;
        arrange(&server, vec![json!({"type": "action.queue.leave"})]).await;

        let got = fetch(
            &client(&server),
            "owner/repo",
            1,
            at("2026-07-30T00:00:00Z"),
        )
        .await
        .unwrap()
        .expect("an event with only a type is still an event");
        assert!(got.dequeued(), "merged defaults to false");
        assert!(got.event.metadata.dequeue_code.is_none());
    }

    #[tokio::test]
    async fn dequeued_is_false_for_a_merge() {
        let server = MockServer::start().await;
        let mut event = checks_failed_event();
        event["metadata"]["merged"] = json!(true);
        event["metadata"]["dequeue_code"] = json!("PR_MERGED");
        arrange(&server, vec![event]).await;

        let got = fetch(
            &client(&server),
            "owner/repo",
            1700,
            at("2026-07-30T00:00:00Z"),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!got.dequeued(), "a merge is a leave, but not a dequeue");
    }

    fn render_with(leave: &LastLeave, pr_number: u64, verbose: bool) -> String {
        let mut buf: Vec<u8> = Vec::new();
        render(
            &mut buf,
            &Theme::new(false),
            leave,
            pr_number,
            at("2026-07-30T00:00:00Z"),
            verbose,
        )
        .unwrap();
        String::from_utf8(buf).unwrap()
    }

    fn render_to_string(leave: &LastLeave, pr_number: u64) -> String {
        render_with(leave, pr_number, false)
    }

    fn leave_from(raw: serde_json::Value) -> LastLeave {
        let event = serde_json::from_value(raw.clone()).unwrap();
        LastLeave { raw, event }
    }

    #[test]
    fn render_dequeue_names_the_code_reason_checks_and_next_step() {
        let rendered = render_to_string(&leave_from(checks_failed_event()), 1700);
        assert!(
            rendered.contains("PR #1700 was dequeued"),
            "got: {rendered}"
        );
        assert!(rendered.contains("9d ago"), "got: {rendered}");
        assert!(rendered.contains("CHECKS_FAILED"), "got: {rendered}");
        assert!(rendered.contains("default"), "got: {rendered}");
        assert!(
            rendered.contains("The merge conditions cannot be satisfied"),
            "got: {rendered}",
        );
        assert!(rendered.contains("ci-gate"), "got: {rendered}");
        assert!(
            rendered.contains("https://github.com/owner/repo/actions/runs/1/job/2"),
            "the failing check's URL is the whole point: {rendered}",
        );
        assert!(rendered.contains("@mergifyio queue"), "got: {rendered}");
    }

    #[test]
    fn render_merge_says_merged_and_offers_no_requeue() {
        let mut raw = checks_failed_event();
        raw["metadata"]["merged"] = json!(true);
        raw["metadata"]["dequeue_code"] = json!("PR_MERGED");
        raw["metadata"]["reason"] = json!("Pull request #1700 has been merged automatically");
        raw["metadata"]["unsuccessful_checks"] = json!([]);

        let rendered = render_to_string(&leave_from(raw), 1700);
        assert!(
            rendered.contains("PR #1700 was merged by the merge queue"),
            "got: {rendered}",
        );
        assert!(
            !rendered.contains("Dequeue code"),
            "PR_MERGED is tautological, not a dequeue reason: {rendered}",
        );
        assert!(
            !rendered.contains("requeue"),
            "nothing to requeue after a merge: {rendered}",
        );
    }

    #[test]
    fn head_sha_is_the_commit_the_diagnosis_describes() {
        // The checks in this event belong to that commit. A consumer
        // comparing it against the PR's current head is what keeps a
        // dequeue already superseded by a push from being reported as
        // a live failure — so the full SHA must survive the decode.
        let leave = leave_from(checks_failed_event());
        assert_eq!(
            leave.head_sha(),
            Some("31b4a485b8ce6f2c1d0e9a7b4c5d6e7f80910111"),
        );
    }

    #[test]
    fn head_sha_is_absent_rather_than_empty_when_the_event_omits_it() {
        let mut raw = checks_failed_event();
        raw["metadata"]["pull_request_head_sha"] = json!("");
        assert!(leave_from(raw).head_sha().is_none());

        let mut raw = checks_failed_event();
        raw["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("pull_request_head_sha");
        assert!(leave_from(raw).head_sha().is_none());
    }

    #[test]
    fn render_abbreviates_the_head_sha() {
        let rendered = render_to_string(&leave_from(checks_failed_event()), 1700);
        assert!(
            rendered.contains("Head SHA:      31b4a48"),
            "got: {rendered}",
        );
    }

    #[test]
    fn render_omits_the_head_sha_row_when_the_event_has_none() {
        let mut raw = checks_failed_event();
        raw["metadata"]["pull_request_head_sha"] = json!(null);
        let rendered = render_to_string(&leave_from(raw), 1700);
        assert!(!rendered.contains("Head SHA"), "got: {rendered}");
    }

    #[test]
    fn render_survives_an_unknown_dequeue_code() {
        // New engine codes must render verbatim, matching the
        // survive-unknown-values stance `check_state_glyph` takes.
        let mut raw = checks_failed_event();
        raw["metadata"]["dequeue_code"] = json!("SOMETHING_NEW_IN_2027");
        let rendered = render_to_string(&leave_from(raw), 1700);
        assert!(
            rendered.contains("SOMETHING_NEW_IN_2027"),
            "got: {rendered}",
        );
    }

    #[test]
    fn render_falls_back_to_url_when_details_url_is_absent() {
        let mut raw = checks_failed_event();
        raw["metadata"]["unsuccessful_checks"][0]["details_url"] = json!(null);
        let rendered = render_to_string(&leave_from(raw), 1700);
        assert!(
            rendered.contains("https://github.com/owner/repo/actions/runs/1/job/2"),
            "got: {rendered}",
        );
    }

    #[test]
    fn render_truncates_a_long_reason_until_verbose() {
        // A real `PR_DEQUEUED` reason embeds the whole unmet
        // condition tree — 60+ lines on `Mergifyio/monorepo` — which
        // would bury the headline and the failing checks.
        let mut raw = checks_failed_event();
        let long: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
        raw["metadata"]["reason"] = json!(long.join("\n"));
        let leave = leave_from(raw);

        let compact = render_with(&leave, 1700, false);
        assert!(compact.contains("line 0"), "got: {compact}");
        assert!(compact.contains("line 11"), "got: {compact}");
        assert!(!compact.contains("line 12"), "got: {compact}");
        assert!(
            compact.contains("… 28 more lines (-v for the full reason)"),
            "truncation must announce itself: {compact}",
        );
        // The next step still lands after the truncated reason.
        assert!(compact.contains("@mergifyio queue"), "got: {compact}");

        let verbose = render_with(&leave, 1700, true);
        assert!(verbose.contains("line 39"), "got: {verbose}");
        assert!(!verbose.contains("more lines"), "got: {verbose}");
    }

    #[test]
    fn render_does_not_announce_truncation_for_a_short_reason() {
        let compact = render_to_string(&leave_from(checks_failed_event()), 1700);
        assert!(!compact.contains("more lines"), "got: {compact}");
    }

    #[test]
    fn render_omits_the_timestamp_when_it_is_unparseable() {
        let mut raw = checks_failed_event();
        raw["received_at"] = json!("not-a-date");
        let rendered = render_to_string(&leave_from(raw), 1700);
        assert!(
            rendered.starts_with("PR #1700 was dequeued\n"),
            "a bad timestamp must not leak into the headline: {rendered}",
        );
    }
}
