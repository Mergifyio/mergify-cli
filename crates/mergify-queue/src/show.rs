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
//! error a script should branch on as an API failure. Under `--json`
//! the not-queued state is a `{"number": N, "queued": false}`
//! document so pipeline consumers always get parseable output; in
//! human mode it is the line `PR #N is not in the merge queue`. Live
//! smoke tests assert against that substring, which is stable across
//! the Python and Rust implementations.

use std::io::Write;

use anstyle::AnsiColor;
use chrono::DateTime;
use chrono::Utc;
use mergify_core::CliError;
use mergify_core::CommandContext;
use mergify_core::Output;
use mergify_tui::StyledGlyph;
use mergify_tui::Theme;
use mergify_tui::relative_time;
use mergify_tui::tree;
use serde::Deserialize;

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
        emit_not_queued(output, opts.pr_number, opts.output_json)?;
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

/// Emit the "PR is not in the merge queue" state. This is a normal
/// answer, not a failure, so the command exits 0 — see the module
/// docs. Under `--json` we emit a `{number, queued: false}` document
/// (a machine consumer always gets parseable output); in human mode
/// a single notice line. The wording is load-bearing: live smoke
/// tests assert on the "is not in the merge queue" substring.
fn emit_not_queued(
    output: &mut dyn Output,
    pr_number: u64,
    output_json: bool,
) -> Result<(), CliError> {
    if output_json {
        let payload = serde_json::json!({ "number": pr_number, "queued": false });
        output.emit_json_value(&payload)?;
    } else {
        output.emit(&(), &mut |w: &mut dyn Write| {
            writeln!(w, "PR #{pr_number} is not in the merge queue")
        })?;
    }
    Ok(())
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

    #[tokio::test]
    async fn run_404_human_is_not_in_queue_and_succeeds() {
        // A not-queued PR is a normal queryable state, not an API
        // failure: human mode prints the notice to stdout and the
        // command returns Ok (exit 0).
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/merge-queue/pull/999"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = Captured::human();
        let api_url = server.uri();
        run(
            ShowOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                pr_number: 999,
                verbose: false,
                output_json: false,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let stdout = cap.stdout();
        assert!(
            stdout.contains("PR #999 is not in the merge queue"),
            "got: {stdout:?}",
        );
    }

    #[tokio::test]
    async fn run_404_json_emits_not_queued_document() {
        // Under `--json`, the not-queued state is a parseable
        // `{number, queued: false}` document on stdout (exit 0), so
        // pipeline consumers never get empty output for the common
        // case.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/merge-queue/pull/999"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = Captured::new(OutputMode::Json);
        let api_url = server.uri();
        run(
            ShowOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                pr_number: 999,
                verbose: false,
                output_json: true,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let stdout = cap.stdout();
        let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(parsed["number"], json!(999));
        assert_eq!(parsed["queued"], json!(false));
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
