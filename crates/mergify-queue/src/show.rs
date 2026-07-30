//! `mergify queue show` — detailed state of a single PR in the
//! merge queue.
//!
//! `GET /v1/repos/<repo>/merge-queue/pull/<pr_number>`. Two output
//! modes:
//!
//! - `--json`: pretty-prints the raw API response as a single JSON
//!   document. The schema is Mergify's API contract, not this CLI's,
//!   so unknown fields are preserved.
//! - Human (default): metadata block (position / priority / queue
//!   rule / queued / ETA), then a CI-state line and a checks
//!   section, then a conditions section. `--verbose` switches the
//!   checks summary to a full table and the conditions summary to
//!   a tree.
//!
//! 404 responses are special-cased: the API returns 404 for "PR is
//! not currently in the merge queue", which is a routine queryable
//! state rather than a server failure. The command reports it on
//! stdout and exits 0 — a not-queued PR is a normal answer, not an
//! error a script should branch on as an API failure.
//!
//! That 404 is also *overloaded*: it is the same answer for a PR
//! dequeued ten minutes ago on failing CI, a PR nobody ever queued,
//! and a PR number that does not exist. So the 404 path asks the
//! activity log for the pull request's last `action.queue.leave` (see
//! [`crate::last_leave`]) and reports which of those three worlds it
//! is in, with the dequeue reason and the failing checks' URLs.
//!
//! Contract, unchanged in both modes: **exit 0**, and under `--json` a
//! `queued: false` document. The JSON gains `dequeued` (`true` /
//! `false` / `null` when the log could not be read), `queue_leave`,
//! the raw event verbatim so unknown fields survive, and
//! `queue_leave_head_sha`, the head that diagnosis describes — without
//! it a consumer cannot tell a live failure from one a later push has
//! already superseded. In human mode a PR with no queue history still
//! prints the exact line `PR #N is not in the merge queue` — live
//! smoke tests assert against that substring, which is a stable
//! contract.

use std::io::Write;

use anstyle::AnsiColor;
use chrono::DateTime;
use chrono::Utc;
use mergify_core::CliError;
use mergify_core::CommandContext;
use mergify_core::Output;
use mergify_core::http::Client;
use mergify_tui::StyledGlyph;
use mergify_tui::Theme;
use mergify_tui::relative_time;
use mergify_tui::tree;
use serde::Deserialize;

use crate::last_leave;
use crate::last_leave::LastLeave;

pub struct ShowOptions<'a> {
    pub repository: Option<&'a str>,
    pub token: Option<&'a str>,
    pub api_url: Option<&'a str>,
    pub pr_number: u64,
    pub verbose: bool,
    pub output_json: bool,
}

#[derive(Deserialize)]
struct PullView {
    number: u64,
    #[serde(default)]
    queued_at: Option<String>,
    #[serde(default)]
    estimated_time_of_merge: Option<String>,
    #[serde(default)]
    position: Option<u64>,
    #[serde(default)]
    priority_rule_name: Option<String>,
    #[serde(default)]
    queue_rule_name: Option<String>,
    /// When the queue will give up on this PR's checks. `--json`
    /// passed it through from the start, but the human render dropped
    /// it — so a `CHECKS_TIMEOUT` could only be diagnosed after the
    /// fact, never seen coming.
    #[serde(default)]
    checks_timeout_at: Option<String>,
    #[serde(default)]
    mergeability_check: Option<MergeabilityCheck>,
}

#[derive(Deserialize)]
struct MergeabilityCheck {
    #[serde(default)]
    check_type: Option<String>,
    #[serde(default)]
    started_at: Option<String>,
    ci_state: String,
    #[serde(default)]
    checks: Vec<Check>,
    #[serde(default)]
    conditions_evaluation: Option<ConditionEvaluation>,
}

#[derive(Deserialize)]
struct Check {
    name: String,
    state: String,
}

#[derive(Deserialize)]
struct ConditionEvaluation {
    #[serde(default)]
    label: String,
    #[serde(default = "default_match_true")]
    r#match: bool,
    #[serde(default)]
    subconditions: Vec<ConditionEvaluation>,
}

// The top-level `conditions_evaluation` payload may legitimately
// omit `match` (it's the aggregator node, not a leaf). Treat a
// missing flag as "matched" so we don't render a spurious failure
// for the root.
const fn default_match_true() -> bool {
    true
}

/// Run the `queue show` command.
pub async fn run(opts: ShowOptions<'_>, output: &mut dyn Output) -> Result<(), CliError> {
    let ctx = CommandContext::resolve(opts.repository, opts.token, opts.api_url)?;

    let client = ctx.mergify_client()?;
    let path = format!(
        "/v1/repos/{repo}/merge-queue/pull/{pr_number}",
        repo = ctx.repository,
        pr_number = opts.pr_number,
    );

    output.status(&format!(
        "Fetching merge queue state for PR #{n}…",
        n = opts.pr_number,
    ))?;

    let raw: Option<serde_json::Value> = client.get_if_exists(&path).await?;
    let Some(raw) = raw else {
        emit_not_queued(&client, &ctx.repository, output, &opts).await?;
        return Ok(());
    };

    if opts.output_json {
        output.emit_json_value(&raw)?;
        return Ok(());
    }

    let view: PullView = serde_json::from_value(raw)
        .map_err(|e| CliError::Generic(format!("decode merge queue pull response: {e}")))?;
    emit_human(output, &view, opts.verbose)?;
    Ok(())
}

/// Emit the "PR is not currently in the merge queue" state, with the
/// diagnosis of *why* when the activity log has one. This is a normal
/// answer, not a failure, so the command exits 0 — see the module
/// docs.
///
/// Reading the activity log is best-effort. A token that can read the
/// merge queue but not the repository's whole event log gets a 403
/// here; turning that into a command failure would break a command
/// that worked before. So a lookup failure degrades to the plain
/// notice plus a stderr warning, and `--json` reports it as
/// `dequeued: null` with a `queue_leave_error` — never as a silent
/// "no dequeue found", which is the very ambiguity this path exists
/// to remove.
async fn emit_not_queued(
    client: &Client,
    repository: &str,
    output: &mut dyn Output,
    opts: &ShowOptions<'_>,
) -> Result<(), CliError> {
    let pr_number = opts.pr_number;
    let now = Utc::now();
    let lookup = last_leave::fetch(client, repository, pr_number, now).await;
    let (leave, error) = match lookup {
        Ok(leave) => (leave, None),
        Err(e) => {
            let message = e.to_string();
            output.status(&format!(
                "could not read the activity log to check for a past dequeue: {message}",
            ))?;
            (None, Some(message))
        }
    };

    if opts.output_json {
        output.emit_json_value(&not_queued_json(
            pr_number,
            leave.as_ref(),
            error.as_deref(),
        ))?;
        return Ok(());
    }

    let theme = Theme::detect();
    output.emit(&(), &mut |w: &mut dyn Write| {
        if let Some(leave) = &leave {
            return last_leave::render(w, &theme, leave, pr_number, now, opts.verbose);
        }
        // The exact wording live smoke tests assert on. It is also
        // the truthful headline when the log lookup failed — the PR
        // is not in the queue either way; the warning already went
        // to stderr.
        writeln!(w, "PR #{pr_number} is not in the merge queue")?;
        if error.is_none() {
            writeln!(w)?;
            last_leave::render_no_activity(w, &theme)?;
        }
        Ok(())
    })?;
    Ok(())
}

/// `--json` payload for the not-queued state. `queued: false` is kept
/// verbatim for back-compat; `dequeued` is the tri-state answer
/// (`true` left without merging, `false` never left / merged, `null`
/// undetermined) and `queue_leave` republishes the raw API event so
/// unknown fields survive, exactly as the queued path does.
///
/// `queue_leave_head_sha` is promoted out of that raw event because a
/// consumer cannot act on the diagnosis without it: the failing checks
/// it reports belong to the head the queue was testing, so a caller
/// that does not compare it against the PR's current head will report
/// a red that a later push has already superseded.
fn not_queued_json(
    pr_number: u64,
    leave: Option<&LastLeave>,
    error: Option<&str>,
) -> serde_json::Value {
    // `null` only when the lookup failed. "No leave event in the
    // retained window" is a real answer — the PR was not dequeued —
    // so it is `false`, not "undetermined".
    let dequeued = match error {
        Some(_) => None,
        None => Some(leave.is_some_and(LastLeave::dequeued)),
    };
    let mut payload = serde_json::json!({
        "number": pr_number,
        "queued": false,
        "dequeued": dequeued,
        "queue_leave_head_sha": leave.and_then(LastLeave::head_sha),
        "queue_leave": leave.map(|l| l.raw.clone()),
    });
    if let Some(error) = error
        && let Some(map) = payload.as_object_mut()
    {
        map.insert("queue_leave_error".to_string(), error.into());
    }
    payload
}

fn emit_human(output: &mut dyn Output, view: &PullView, verbose: bool) -> std::io::Result<()> {
    let now = Utc::now();
    let theme = Theme::detect();
    output.emit(&(), &mut |w: &mut dyn Write| {
        print_metadata(w, &theme, view, now)?;

        match &view.mergeability_check {
            None => {
                writeln!(w)?;
                writeln!(
                    w,
                    "  {D}Waiting for mergeability check...{R}",
                    D = theme.dim,
                    R = theme.reset,
                )?;
            }
            Some(mc) => {
                print_checks_section(w, &theme, mc, verbose, now)?;
                if let Some(conditions) = &mc.conditions_evaluation {
                    print_conditions_section(w, &theme, conditions, verbose)?;
                }
            }
        }
        Ok(())
    })
}

fn print_metadata(
    w: &mut dyn Write,
    theme: &Theme,
    view: &PullView,
    now: DateTime<Utc>,
) -> std::io::Result<()> {
    writeln!(
        w,
        "{B}PR #{n}{R}",
        B = theme.bold,
        n = view.number,
        R = theme.reset,
    )?;
    writeln!(w)?;
    writeln!(
        w,
        "  Position:    {}",
        display_or_dash(view.position.map(|n| n.to_string()).as_deref()),
    )?;
    writeln!(
        w,
        "  Priority:    {}",
        display_or_dash(view.priority_rule_name.as_deref()),
    )?;
    writeln!(
        w,
        "  Queue rule:  {}",
        display_or_dash(view.queue_rule_name.as_deref()),
    )?;
    writeln!(
        w,
        "  Queued at:   {}",
        relative_or_raw_or_dash(view.queued_at.as_deref(), now, false),
    )?;
    writeln!(
        w,
        "  ETA:         {}",
        relative_or_raw_or_dash(view.estimated_time_of_merge.as_deref(), now, true),
    )?;
    writeln!(
        w,
        "  CI timeout:  {}",
        relative_or_raw_or_dash(view.checks_timeout_at.as_deref(), now, true),
    )
}

fn display_or_dash(value: Option<&str>) -> &str {
    value.filter(|s| !s.is_empty()).unwrap_or("-")
}

fn relative_or_raw_or_dash(value: Option<&str>, now: DateTime<Utc>, future: bool) -> String {
    let Some(raw) = value else {
        return "-".to_string();
    };
    let rel = relative_time(raw, now, future);
    if rel.is_empty() {
        // Unparseable timestamp — show the raw string so the user
        // sees *something* rather than a silent dash.
        raw.to_string()
    } else {
        rel
    }
}

fn print_checks_section(
    w: &mut dyn Write,
    theme: &Theme,
    mc: &MergeabilityCheck,
    verbose: bool,
    now: DateTime<Utc>,
) -> std::io::Result<()> {
    writeln!(w)?;
    let glyph = check_state_glyph(theme, &mc.ci_state);
    write!(
        w,
        "  CI State: {S}{icon} {state}{R}",
        S = glyph.style,
        icon = glyph.icon,
        state = mc.ci_state,
        R = theme.reset,
    )?;
    if let Some(check_type) = mc.check_type.as_deref().filter(|s| !s.is_empty()) {
        write!(w, "   {D}{check_type}{R}", D = theme.dim, R = theme.reset)?;
    }
    if let Some(started) = &mc.started_at {
        let rel = relative_time(started, now, false);
        if !rel.is_empty() {
            write!(w, "   {D}started {rel}{R}", D = theme.dim, R = theme.reset)?;
        }
    }
    writeln!(w)?;

    if mc.checks.is_empty() {
        return Ok(());
    }

    if verbose {
        print_checks_table(w, theme, &mc.checks)
    } else {
        print_checks_summary(w, theme, &mc.checks)
    }
}

fn print_checks_table(w: &mut dyn Write, theme: &Theme, checks: &[Check]) -> std::io::Result<()> {
    // First column carries the `  Check` header, so its width is the
    // wider of the padded check names and the header label itself
    // (mirrors rich's auto-sizing of the "  Check" column).
    const HEADER_CHECK: &str = "  Check";
    let name_col_width = checks
        .iter()
        .map(|c| 2 + c.name.chars().count())
        .max()
        .unwrap_or(0)
        .max(HEADER_CHECK.chars().count());

    // Header row: `Check` / `Status`, dim, matching Python's
    // `Table(show_header=True)` column titles.
    let header_pad = name_col_width.saturating_sub(HEADER_CHECK.chars().count());
    writeln!(
        w,
        "{D}{check}{spaces}  Status{R}",
        D = theme.dim,
        check = HEADER_CHECK,
        spaces = " ".repeat(header_pad),
        R = theme.reset,
    )?;

    for check in checks {
        let glyph = check_state_glyph(theme, &check.state);
        let pad = name_col_width.saturating_sub(2 + check.name.chars().count());
        writeln!(
            w,
            "  {D}{name}{spaces}{R}  {S}{icon} {state}{R}",
            D = theme.dim,
            name = check.name,
            spaces = " ".repeat(pad),
            R = theme.reset,
            S = glyph.style,
            icon = glyph.icon,
            state = check.state,
        )?;
    }
    Ok(())
}

fn print_checks_summary(w: &mut dyn Write, theme: &Theme, checks: &[Check]) -> std::io::Result<()> {
    let mut passed: u32 = 0;
    let mut pending: u32 = 0;
    let mut failed: u32 = 0;
    for check in checks {
        match check.state.as_str() {
            "success" | "neutral" | "skipped" => passed += 1,
            "pending" => pending += 1,
            _ => failed += 1,
        }
    }

    write!(w, "  Checks:  ")?;
    write!(
        w,
        "{S}{passed} passed{R}",
        S = theme.fg(AnsiColor::Green),
        R = theme.reset,
    )?;
    if pending > 0 {
        write!(
            w,
            ", {S}{pending} pending{R}",
            S = theme.fg(AnsiColor::Blue),
            R = theme.reset,
        )?;
    }
    if failed > 0 {
        write!(
            w,
            ", {S}{failed} failed{R}",
            S = theme.fg(AnsiColor::Red),
            R = theme.reset,
        )?;
    }
    writeln!(w)?;

    for check in checks {
        if matches!(
            check.state.as_str(),
            "failure" | "error" | "timed_out" | "action_required"
        ) {
            let glyph = check_state_glyph(theme, &check.state);
            writeln!(
                w,
                "    {S}{icon} {state}{R}  {D}{name}{R}",
                S = glyph.style,
                icon = glyph.icon,
                state = check.state,
                R = theme.reset,
                D = theme.dim,
                name = check.name,
            )?;
        }
    }
    Ok(())
}

/// Map a check state string to its [`StyledGlyph`], using the
/// single-width terminal vocabulary (✓ ✗ ● ○ —); unknown states fall
/// back to a dim `—` so the renderer never crashes on a new API code.
fn check_state_glyph(theme: &Theme, state: &str) -> StyledGlyph {
    match state {
        "success" => StyledGlyph::new("✓", theme.fg(AnsiColor::Green)),
        "pending" => StyledGlyph::new("●", theme.fg(AnsiColor::Yellow)),
        "failure" | "error" | "action_required" | "timed_out" => {
            StyledGlyph::new("✗", theme.fg(AnsiColor::Red))
        }
        "cancelled" | "neutral" | "skipped" | "stale" => StyledGlyph::new("○", theme.dim),
        _ => StyledGlyph::new("—", theme.dim),
    }
}

fn print_conditions_section(
    w: &mut dyn Write,
    theme: &Theme,
    evaluation: &ConditionEvaluation,
    verbose: bool,
) -> std::io::Result<()> {
    writeln!(w)?;
    if verbose {
        writeln!(w, "{B}Conditions{R}", B = theme.bold, R = theme.reset)?;
        write_condition_tree(w, theme, &evaluation.subconditions, "")?;
        return Ok(());
    }

    let top = &evaluation.subconditions;
    if top.is_empty() {
        return Ok(());
    }

    let met = top.iter().filter(|s| s.r#match).count();
    let total = top.len();
    let style = if met == total {
        theme.fg(AnsiColor::Green)
    } else {
        theme.fg(AnsiColor::Yellow)
    };
    writeln!(
        w,
        "  Conditions: {S}{met}/{total} met{R}",
        S = style,
        R = theme.reset,
    )?;

    for sub in top {
        if sub.r#match {
            continue;
        }
        let summary = if sub.subconditions.is_empty() {
            sub.label.clone()
        } else {
            summarize_failing_group(sub)
        };
        writeln!(
            w,
            "  {S}✗{R} {summary}",
            S = theme.fg(AnsiColor::Red),
            R = theme.reset,
        )?;
    }
    Ok(())
}

fn summarize_failing_group(evaluation: &ConditionEvaluation) -> String {
    let labels: Vec<String> = evaluation.subconditions.iter().map(child_label).collect();
    if labels.len() <= 3 {
        labels.join(" or ")
    } else {
        let head: Vec<&str> = labels.iter().take(2).map(String::as_str).collect();
        format!("{} or ({} more)", head.join(" or "), labels.len() - 2)
    }
}

fn child_label(evaluation: &ConditionEvaluation) -> String {
    let label = &evaluation.label;
    if !is_aggregator(label) {
        return label.clone();
    }
    let Some(first) = evaluation.subconditions.first() else {
        return label.clone();
    };
    if is_aggregator(&first.label) {
        child_label(first)
    } else {
        first.label.clone()
    }
}

fn is_aggregator(label: &str) -> bool {
    matches!(label, "all of" | "any of" | "not")
}

fn write_condition_tree(
    w: &mut dyn Write,
    theme: &Theme,
    nodes: &[ConditionEvaluation],
    prefix: &str,
) -> std::io::Result<()> {
    if nodes.is_empty() {
        return Ok(());
    }
    let last = nodes.len() - 1;
    for (i, node) in nodes.iter().enumerate() {
        let (branch, continuation) = tree::branch_chars(i == last);
        let glyph = if node.r#match {
            StyledGlyph::new("✓", theme.fg(AnsiColor::Green))
        } else {
            StyledGlyph::new("✗", theme.fg(AnsiColor::Red))
        };
        writeln!(
            w,
            "{prefix}{branch}{S}{icon}{R} {label}",
            S = glyph.style,
            icon = glyph.icon,
            R = theme.reset,
            label = node.label,
        )?;
        let child_prefix = format!("{prefix}{continuation}");
        write_condition_tree(w, theme, &node.subconditions, &child_prefix)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use mergify_core::OutputMode;
    use mergify_test_support::Captured;
    use serde_json::json;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::header;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;

    fn pull_response() -> serde_json::Value {
        json!({
            "number": 123,
            "queued_at": "2026-05-09T10:00:00Z",
            "estimated_time_of_merge": "2026-05-09T11:00:00Z",
            "position": 3,
            "priority_rule_name": "default",
            "queue_rule_name": "default",
            "checks_timeout_at": "2026-05-09T12:00:00Z",
            "queue_rule": {"name": "default", "config": {}},
            "mergeability_check": {
                "check_type": "in_place",
                "queue_pull_request_number": 123,
                "started_at": "2026-05-09T10:05:00Z",
                "ci_state": "pending",
                "state": "running",
                "checks": [
                    {"name": "tests", "description": "", "state": "success"},
                    {"name": "linters", "description": "", "state": "pending"},
                    {"name": "security", "description": "", "state": "failure"},
                ],
                "conditions_evaluation": {
                    "match": false,
                    "label": "all of",
                    "subconditions": [
                        {
                            "match": true,
                            "label": "#check-success=tests",
                            "subconditions": [],
                        },
                        {
                            "match": false,
                            "label": "#check-success=linters",
                            "subconditions": [],
                        },
                    ],
                },
            },
        })
    }

    async fn arrange(server: &MockServer, body: serde_json::Value, status: u16) {
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/merge-queue/pull/123"))
            .and(header("Authorization", "Bearer t"))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn run_renders_metadata_and_compact_sections() {
        let server = MockServer::start().await;
        arrange(&server, pull_response(), 200).await;

        let mut cap = Captured::human();
        let api_url = server.uri();
        run(
            ShowOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                pr_number: 123,
                verbose: false,
                output_json: false,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let stdout = cap.stdout();
        assert!(stdout.contains("PR #123"), "got: {stdout:?}");
        assert!(stdout.contains("Position:"), "got: {stdout:?}");
        // A pending checks timeout is visible before it fires, not
        // only diagnosable as CHECKS_TIMEOUT afterwards.
        assert!(stdout.contains("CI timeout:"), "got: {stdout:?}");
        assert!(stdout.contains("CI State:"), "got: {stdout:?}");
        // Compact summary: 1 passed (tests), 1 pending (linters), 1
        // failed (security). The failing check name is listed below
        // the summary line.
        assert!(stdout.contains("1 passed"), "got: {stdout:?}");
        assert!(stdout.contains("1 pending"), "got: {stdout:?}");
        assert!(stdout.contains("1 failed"), "got: {stdout:?}");
        assert!(stdout.contains("security"), "got: {stdout:?}");
        // Compact conditions: "1/2 met" + the failing label.
        assert!(stdout.contains("1/2 met"), "got: {stdout:?}");
        assert!(stdout.contains("#check-success=linters"), "got: {stdout:?}");
    }

    #[tokio::test]
    async fn run_renders_verbose_table_and_tree() {
        let server = MockServer::start().await;
        arrange(&server, pull_response(), 200).await;

        let mut cap = Captured::human();
        let api_url = server.uri();
        run(
            ShowOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                pr_number: 123,
                verbose: true,
                output_json: false,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let stdout = cap.stdout();
        // Verbose table: header row labels both columns.
        assert!(stdout.contains("Check"), "got: {stdout:?}");
        assert!(stdout.contains("Status"), "got: {stdout:?}");
        // Verbose table: every check name appears as its own row.
        assert!(stdout.contains("tests"), "got: {stdout:?}");
        assert!(stdout.contains("linters"), "got: {stdout:?}");
        assert!(stdout.contains("security"), "got: {stdout:?}");
        // Verbose conditions: tree header + box-drawing characters.
        assert!(stdout.contains("Conditions"), "got: {stdout:?}");
        assert!(
            stdout.contains("├──") || stdout.contains("└──"),
            "got: {stdout:?}"
        );
    }

    #[tokio::test]
    async fn run_emits_json_passthrough() {
        let server = MockServer::start().await;
        // Add a synthetic field to verify unknown fields survive
        // the round-trip.
        let mut body = pull_response();
        body["future_field"] = json!("preserved");
        arrange(&server, body, 200).await;

        let mut cap = Captured::new(OutputMode::Json);
        let api_url = server.uri();
        run(
            ShowOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                pr_number: 123,
                verbose: false,
                output_json: true,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let stdout = cap.stdout();
        let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(parsed["number"], json!(123));
        assert_eq!(parsed["future_field"], json!("preserved"));
    }

    /// Mock the merge-queue 404 that sends `queue show` to the
    /// activity-log fallback.
    async fn arrange_not_queued(server: &MockServer, pr_number: u64) {
        Mock::given(method("GET"))
            .and(path(format!(
                "/v1/repos/owner/repo/merge-queue/pull/{pr_number}"
            )))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(server)
            .await;
    }

    /// Mock the activity-log lookup with `events`.
    async fn arrange_logs(server: &MockServer, events: Vec<serde_json::Value>) {
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/logs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "size": events.len(),
                "per_page": 1,
                "events": events,
            })))
            .expect(1)
            .mount(server)
            .await;
    }

    fn dequeue_event() -> serde_json::Value {
        json!({
            "id": 1,
            "received_at": "2026-07-20T23:25:11.263987Z",
            "trigger": "merge queue internal",
            "repository": "owner/repo",
            "pull_request": 999,
            "base_ref": "main",
            "outcome": "failure",
            "type": "action.queue.leave",
            "metadata": {
                "reason": "The merge conditions cannot be satisfied due to failing checks",
                "merged": false,
                "queue_name": "default",
                "queued_at": "2026-07-20T23:11:42.944734Z",
                "pull_request_head_sha": "31b4a485b8ce6f2c1d0e9a7b4c5d6e7f80910111",
                "dequeue_code": "CHECKS_FAILED",
                "unsuccessful_checks": [{
                    "name": "ci-gate",
                    "state": "failure",
                    "details_url": "https://github.com/owner/repo/actions/runs/1/job/2",
                }],
            },
        })
    }

    async fn run_not_queued(server: &MockServer, pr_number: u64, output_json: bool) -> Captured {
        let mut cap = if output_json {
            Captured::new(OutputMode::Json)
        } else {
            Captured::human()
        };
        let api_url = server.uri();
        run(
            ShowOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                pr_number,
                verbose: false,
                output_json,
            },
            &mut cap.output,
        )
        .await
        .unwrap();
        cap
    }

    #[tokio::test]
    async fn run_404_human_is_not_in_queue_and_succeeds() {
        // A PR with no queue history at all: a normal queryable
        // state, not an API failure. Human mode prints the notice on
        // stdout and the command returns Ok (exit 0). The wording is
        // pinned by live smoke tests.
        let server = MockServer::start().await;
        arrange_not_queued(&server, 999).await;
        arrange_logs(&server, vec![]).await;

        let cap = run_not_queued(&server, 999, false).await;
        let stdout = cap.stdout();
        assert!(
            stdout.contains("PR #999 is not in the merge queue"),
            "got: {stdout:?}",
        );
        // "we looked and found nothing" is an answer; silence is not.
        assert!(
            stdout.contains("No merge-queue activity"),
            "got: {stdout:?}",
        );
    }

    #[tokio::test]
    async fn run_404_json_emits_not_queued_document() {
        // Under `--json`, the not-queued state is a parseable
        // `{number, queued: false}` document on stdout (exit 0), so
        // pipeline consumers never get empty output for the common
        // case. `queued: false` is back-compat and must not move.
        let server = MockServer::start().await;
        arrange_not_queued(&server, 999).await;
        arrange_logs(&server, vec![]).await;

        let cap = run_not_queued(&server, 999, true).await;
        let parsed: serde_json::Value = serde_json::from_str(&cap.stdout()).unwrap();
        assert_eq!(parsed["number"], json!(999));
        assert_eq!(parsed["queued"], json!(false));
        assert_eq!(parsed["dequeued"], json!(false));
        assert_eq!(parsed["queue_leave"], json!(null));
    }

    #[tokio::test]
    async fn run_404_human_reports_the_dequeue_reason_and_check_urls() {
        // The gap this fallback closes: one line used to cover
        // "dequeued 10 minutes ago on failing CI" and "never queued"
        // alike.
        let server = MockServer::start().await;
        arrange_not_queued(&server, 999).await;
        arrange_logs(&server, vec![dequeue_event()]).await;

        let cap = run_not_queued(&server, 999, false).await;
        let stdout = cap.stdout();
        assert!(stdout.contains("PR #999 was dequeued"), "got: {stdout:?}");
        assert!(stdout.contains("CHECKS_FAILED"), "got: {stdout:?}");
        assert!(
            stdout.contains("The merge conditions cannot be satisfied"),
            "got: {stdout:?}",
        );
        assert!(
            stdout.contains("https://github.com/owner/repo/actions/runs/1/job/2"),
            "got: {stdout:?}",
        );
        assert!(stdout.contains("@mergifyio queue"), "got: {stdout:?}");
    }

    #[tokio::test]
    async fn run_404_json_reports_dequeued_with_the_raw_event() {
        let server = MockServer::start().await;
        arrange_not_queued(&server, 999).await;
        arrange_logs(&server, vec![dequeue_event()]).await;

        let cap = run_not_queued(&server, 999, true).await;
        let parsed: serde_json::Value = serde_json::from_str(&cap.stdout()).unwrap();
        assert_eq!(parsed["queued"], json!(false));
        assert_eq!(parsed["dequeued"], json!(true));
        // The event is republished verbatim — the schema is
        // Mergify's contract, not this CLI's.
        assert_eq!(parsed["queue_leave"], dequeue_event());
    }

    #[tokio::test]
    async fn run_404_json_promotes_the_head_sha_the_diagnosis_describes() {
        // Without this, a consumer reading `unsuccessful_checks` has
        // no way to notice the checks belong to a commit a later push
        // replaced, and reports a red the PR no longer has. Full SHA,
        // not the abbreviation the human render uses — the caller
        // compares it against the PR head.
        let server = MockServer::start().await;
        arrange_not_queued(&server, 999).await;
        arrange_logs(&server, vec![dequeue_event()]).await;

        let cap = run_not_queued(&server, 999, true).await;
        let parsed: serde_json::Value = serde_json::from_str(&cap.stdout()).unwrap();
        assert_eq!(
            parsed["queue_leave_head_sha"],
            json!("31b4a485b8ce6f2c1d0e9a7b4c5d6e7f80910111"),
        );
    }

    #[tokio::test]
    async fn run_404_json_head_sha_is_null_when_there_is_no_leave_event() {
        // The key is always present, like `dequeued` and
        // `queue_leave`, so a consumer can read it unconditionally.
        let server = MockServer::start().await;
        arrange_not_queued(&server, 999).await;
        arrange_logs(&server, vec![]).await;

        let cap = run_not_queued(&server, 999, true).await;
        let parsed: serde_json::Value = serde_json::from_str(&cap.stdout()).unwrap();
        assert_eq!(parsed["queue_leave_head_sha"], json!(null));
    }

    #[tokio::test]
    async fn run_404_reports_a_merge_as_merged_not_dequeued() {
        // A merge is also an `action.queue.leave`. Reporting it as a
        // dequeue would tell an agent to requeue a merged PR.
        let server = MockServer::start().await;
        let mut event = dequeue_event();
        event["metadata"]["merged"] = json!(true);
        event["metadata"]["dequeue_code"] = json!("PR_MERGED");
        event["metadata"]["unsuccessful_checks"] = json!([]);
        arrange_not_queued(&server, 999).await;
        arrange_logs(&server, vec![event]).await;

        let cap = run_not_queued(&server, 999, false).await;
        let stdout = cap.stdout();
        assert!(
            stdout.contains("PR #999 was merged by the merge queue"),
            "got: {stdout:?}",
        );
        assert!(!stdout.contains("dequeued"), "got: {stdout:?}");
    }

    #[tokio::test]
    async fn run_404_json_reports_a_merge_as_not_dequeued() {
        let server = MockServer::start().await;
        let mut event = dequeue_event();
        event["metadata"]["merged"] = json!(true);
        arrange_not_queued(&server, 999).await;
        arrange_logs(&server, vec![event]).await;

        let cap = run_not_queued(&server, 999, true).await;
        let parsed: serde_json::Value = serde_json::from_str(&cap.stdout()).unwrap();
        assert_eq!(parsed["dequeued"], json!(false));
        assert_eq!(parsed["queue_leave"]["metadata"]["merged"], json!(true));
    }

    #[tokio::test]
    async fn run_404_degrades_when_the_activity_log_is_unreadable() {
        // A queue-scoped token may be refused the repository's event
        // log. `queue show` worked before this fallback existed and
        // must keep working: exit 0, the notice on stdout, the reason
        // on stderr — and never a silent "no dequeue found".
        let server = MockServer::start().await;
        arrange_not_queued(&server, 999).await;
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/logs"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({"detail": "nope"})))
            .expect(1)
            .mount(&server)
            .await;

        let cap = run_not_queued(&server, 999, false).await;
        let stdout = cap.stdout();
        assert!(
            stdout.contains("PR #999 is not in the merge queue"),
            "got: {stdout:?}",
        );
        assert!(
            !stdout.contains("No merge-queue activity"),
            "must not claim we looked when the lookup failed: {stdout:?}",
        );
        assert!(
            cap.stderr().contains("could not read the activity log"),
            "got: {:?}",
            cap.stderr(),
        );
    }

    #[tokio::test]
    async fn run_404_json_reports_an_unreadable_log_as_undetermined() {
        let server = MockServer::start().await;
        arrange_not_queued(&server, 999).await;
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/logs"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({"detail": "nope"})))
            .expect(1)
            .mount(&server)
            .await;

        let cap = run_not_queued(&server, 999, true).await;
        let parsed: serde_json::Value = serde_json::from_str(&cap.stdout()).unwrap();
        assert_eq!(parsed["queued"], json!(false));
        // `null`, not `false`: "we could not tell" is not "it was
        // never dequeued".
        assert_eq!(parsed["dequeued"], json!(null));
        assert!(
            parsed["queue_leave_error"]
                .as_str()
                .is_some_and(|e| e.contains("403")),
            "got: {parsed}",
        );
    }

    #[tokio::test]
    async fn run_no_mergeability_check() {
        let server = MockServer::start().await;
        let body = json!({
            "number": 123,
            "queued_at": "2026-05-09T10:00:00Z",
            "position": 1,
            "priority_rule_name": "default",
            "queue_rule_name": "default",
            "queue_rule": {"name": "default", "config": {}},
            "mergeability_check": null,
        });
        arrange(&server, body, 200).await;

        let mut cap = Captured::human();
        let api_url = server.uri();
        run(
            ShowOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                pr_number: 123,
                verbose: false,
                output_json: false,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let stdout = cap.stdout();
        assert!(
            stdout.contains("Waiting for mergeability check"),
            "got: {stdout:?}",
        );
    }

    #[test]
    fn summarize_failing_group_two_labels() {
        let group = ConditionEvaluation {
            label: "any of".to_string(),
            r#match: false,
            subconditions: vec![
                ConditionEvaluation {
                    label: "a".to_string(),
                    r#match: false,
                    subconditions: vec![],
                },
                ConditionEvaluation {
                    label: "b".to_string(),
                    r#match: false,
                    subconditions: vec![],
                },
            ],
        };
        assert_eq!(summarize_failing_group(&group), "a or b");
    }

    #[test]
    fn summarize_failing_group_truncates_at_three_plus() {
        let group = ConditionEvaluation {
            label: "any of".to_string(),
            r#match: false,
            subconditions: vec![
                ConditionEvaluation {
                    label: "a".to_string(),
                    r#match: false,
                    subconditions: vec![],
                },
                ConditionEvaluation {
                    label: "b".to_string(),
                    r#match: false,
                    subconditions: vec![],
                },
                ConditionEvaluation {
                    label: "c".to_string(),
                    r#match: false,
                    subconditions: vec![],
                },
                ConditionEvaluation {
                    label: "d".to_string(),
                    r#match: false,
                    subconditions: vec![],
                },
            ],
        };
        // 4 items: keep first 2, summarize the rest.
        assert_eq!(summarize_failing_group(&group), "a or b or (2 more)");
    }

    #[test]
    fn child_label_recurses_through_aggregators() {
        let nested = ConditionEvaluation {
            label: "any of".to_string(),
            r#match: false,
            subconditions: vec![ConditionEvaluation {
                label: "all of".to_string(),
                r#match: false,
                subconditions: vec![ConditionEvaluation {
                    label: "leaf".to_string(),
                    r#match: false,
                    subconditions: vec![],
                }],
            }],
        };
        assert_eq!(child_label(&nested), "leaf");
    }
}
