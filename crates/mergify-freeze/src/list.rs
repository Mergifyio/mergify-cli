//! `mergify freeze list` — list scheduled freezes for a repository.
//!
//! `GET /v1/repos/<repo>/scheduled_freeze`. Two output modes:
//!
//! - `--json`: pretty-prints the inner `scheduled_freezes` array
//!   verbatim. Mirrors Python's `list_cmd`, which returns
//!   `response.json()["scheduled_freezes"]` and feeds it to
//!   `json.dumps`. The schema is Mergify's API contract, not this
//!   CLI's, so unknown fields survive the round trip.
//! - Human (default): a table with columns ID / Reason / Start /
//!   End / Conditions / Status. The status column is a best-effort
//!   active-vs-scheduled flag: the API returns `start` as a naive
//!   timestamp in the freeze's own timezone, but we compare against
//!   UTC `now` because we don't have the server's local clock. Same
//!   approximation Python's `_is_active` makes.

use std::io::Write;

use anstyle::AnsiColor;
use chrono::DateTime;
use chrono::Utc;
use mergify_core::ApiFlavor;
use mergify_core::CliError;
use mergify_core::HttpClient;
use mergify_core::Output;
use mergify_core::auth;
use mergify_tui::Theme;

use crate::common::ScheduledFreeze;
use crate::common::format_datetime;
use crate::common::parse_naive_datetime;

pub struct ListOptions<'a> {
    pub repository: Option<&'a str>,
    pub token: Option<&'a str>,
    pub api_url: Option<&'a str>,
    pub output_json: bool,
}

/// Run the `freeze list` command.
pub async fn run(opts: ListOptions<'_>, output: &mut dyn Output) -> Result<(), CliError> {
    let repository = auth::resolve_repository(opts.repository)?;
    let token = auth::resolve_token(opts.token)?;
    let api_url = auth::resolve_api_url(opts.api_url)?;

    output.status(&format!("Fetching scheduled freezes for {repository}…"))?;

    let client = HttpClient::new(api_url, token, ApiFlavor::Mergify)?;
    let path = format!("/v1/repos/{repository}/scheduled_freeze");
    let raw: serde_json::Value = client.get(&path).await?;

    // Python's `list_freezes` returns `data["scheduled_freezes"]`
    // and the CLI prints that inner array verbatim. Treat a missing
    // key as an empty list so a future server quirk doesn't 500
    // the renderer.
    let freezes = raw
        .get("scheduled_freezes")
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));

    if opts.output_json {
        output.emit_json_value(&freezes)?;
        return Ok(());
    }

    let views: Vec<ScheduledFreeze> = serde_json::from_value(freezes)
        .map_err(|e| CliError::Generic(format!("decode scheduled freezes response: {e}")))?;
    emit_human(output, &views, Utc::now())?;
    Ok(())
}

fn emit_human(
    output: &mut dyn Output,
    freezes: &[ScheduledFreeze],
    now: DateTime<Utc>,
) -> std::io::Result<()> {
    let theme = Theme::detect();
    output.emit(&(), &mut |w: &mut dyn Write| {
        if freezes.is_empty() {
            writeln!(w, "No scheduled freezes found.")?;
            return Ok(());
        }
        render_table(w, &theme, freezes, now)
    })
}

const HEADERS: [&str; 6] = ["ID", "Reason", "Start", "End", "Conditions", "Status"];

fn render_table(
    w: &mut dyn Write,
    theme: &Theme,
    freezes: &[ScheduledFreeze],
    now: DateTime<Utc>,
) -> std::io::Result<()> {
    writeln!(
        w,
        "{B}Scheduled Freezes{R}",
        B = theme.bold,
        R = theme.reset
    )?;
    writeln!(w)?;

    let rows: Vec<[String; 6]> = freezes.iter().map(|f| row_for(f, now)).collect();

    let widths = column_widths(&rows);

    write_row(w, theme, &HEADERS.map(String::from), &widths, true)?;
    write_separator(w, &widths)?;
    for row in &rows {
        write_row(w, theme, row, &widths, false)?;
    }
    Ok(())
}

fn row_for(freeze: &ScheduledFreeze, now: DateTime<Utc>) -> [String; 6] {
    let id = freeze.id.clone().unwrap_or_default();
    let reason = freeze.reason.clone().unwrap_or_default();
    let timezone = freeze.timezone.as_deref().unwrap_or("");
    let start = format_datetime(freeze.start.as_deref(), timezone);
    let end = format_datetime(freeze.end.as_deref(), timezone);
    let conditions = format_conditions(&freeze.matching_conditions, &freeze.exclude_conditions);
    let status = if is_active(freeze.start.as_deref(), now) {
        "active".to_string()
    } else {
        "scheduled".to_string()
    };
    [id, reason, start, end, conditions, status]
}

/// Build the Conditions cell: matching conditions joined with `, `,
/// followed by `(exclude: …)` when any exclude conditions are set.
/// Same formatting as Python's `_print_freeze_table`.
fn format_conditions(matching: &[String], exclude: &[String]) -> String {
    let mut out = matching.join(", ");
    if !exclude.is_empty() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str("(exclude: ");
        out.push_str(&exclude.join(", "));
        out.push(')');
    }
    out
}

/// Best-effort active flag — see module docs for the timezone caveat
/// (same one the Python implementation acknowledges). Delegates the
/// ISO-8601 parse to [`parse_naive_datetime`] so the CLI-input parser
/// and the API-response parser stay locked. A malformed `start` falls
/// through to "scheduled" instead of aborting the render.
fn is_active(start: Option<&str>, now: DateTime<Utc>) -> bool {
    let Some(start) = start else {
        return false;
    };
    let Ok(naive_start) = parse_naive_datetime(start) else {
        return false;
    };
    naive_start <= now.naive_utc()
}

fn column_widths(rows: &[[String; 6]]) -> [usize; 6] {
    let mut widths = HEADERS.map(str::len);
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    widths
}

fn write_row(
    w: &mut dyn Write,
    theme: &Theme,
    row: &[String; 6],
    widths: &[usize; 6],
    header: bool,
) -> std::io::Result<()> {
    for (i, cell) in row.iter().enumerate() {
        if i > 0 {
            write!(w, "  ")?;
        }
        let pad = widths[i].saturating_sub(cell.chars().count());
        if header {
            write!(
                w,
                "{B}{cell}{R}{spaces}",
                B = theme.bold,
                R = theme.reset,
                spaces = " ".repeat(pad),
            )?;
        } else if HEADERS[i] == "Status" {
            let style = if cell == "active" {
                if theme.enabled {
                    theme.fg(AnsiColor::Green)
                } else {
                    anstyle::Style::new()
                }
            } else if theme.enabled {
                theme.fg(AnsiColor::Yellow)
            } else {
                anstyle::Style::new()
            };
            write!(
                w,
                "{S}{cell}{R}{spaces}",
                S = style,
                R = theme.reset,
                spaces = " ".repeat(pad),
            )?;
        } else {
            write!(w, "{cell}{spaces}", spaces = " ".repeat(pad))?;
        }
    }
    writeln!(w)
}

fn write_separator(w: &mut dyn Write, widths: &[usize; 6]) -> std::io::Result<()> {
    for (i, width) in widths.iter().enumerate() {
        if i > 0 {
            write!(w, "  ")?;
        }
        write!(w, "{}", "─".repeat(*width))?;
    }
    writeln!(w)
}

#[cfg(test)]
mod tests {
    use mergify_core::OutputMode;
    use mergify_core::StdioOutput;
    use serde_json::json;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::header;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;

    type SharedBytes = std::sync::Arc<std::sync::Mutex<Vec<u8>>>;

    struct Captured {
        output: StdioOutput,
        stdout: SharedBytes,
    }

    fn make_output(mode: OutputMode) -> Captured {
        let stdout: SharedBytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let stderr: SharedBytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let output = StdioOutput::with_sinks(
            mode,
            SharedWriter(std::sync::Arc::clone(&stdout)),
            SharedWriter(std::sync::Arc::clone(&stderr)),
        );
        Captured { output, stdout }
    }

    fn stdout_string(cap: &Captured) -> String {
        String::from_utf8(cap.stdout.lock().unwrap().clone()).unwrap()
    }

    struct SharedWriter(SharedBytes);
    impl Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn freeze_sample() -> serde_json::Value {
        json!({
            "id": "11111111-2222-3333-4444-555555555555",
            "reason": "emergency-fix",
            "start": "2026-01-01T10:00:00",
            "end": "2026-01-01T12:00:00",
            "timezone": "Europe/Paris",
            "matching_conditions": ["base=main"],
            "exclude_conditions": ["label=hotfix"],
        })
    }

    async fn arrange(server: &MockServer, body: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/scheduled_freeze"))
            .and(header("Authorization", "Bearer t"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn run_json_passthrough_emits_inner_array() {
        // Python's `list_freezes` returns `data["scheduled_freezes"]`
        // — the inner array, not the wrapping object. The Rust port
        // must preserve that contract: `--json` mode prints exactly
        // the array, including any unknown fields on the freeze
        // objects.
        let server = MockServer::start().await;
        let mut freeze = freeze_sample();
        freeze["future_field"] = json!("preserved");
        let body = json!({"scheduled_freezes": [freeze.clone()]});
        arrange(&server, body).await;

        let mut cap = make_output(OutputMode::Json);
        let api_url = server.uri();
        run(
            ListOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                output_json: true,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let stdout = stdout_string(&cap);
        let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(parsed, json!([freeze]));
    }

    #[tokio::test]
    async fn run_json_passthrough_empty_array() {
        // Server returns an empty list — JSON mode must still emit
        // `[]` (not `null`, not the wrapping object).
        let server = MockServer::start().await;
        arrange(&server, json!({"scheduled_freezes": []})).await;

        let mut cap = make_output(OutputMode::Json);
        let api_url = server.uri();
        run(
            ListOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                output_json: true,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let stdout = stdout_string(&cap);
        let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(parsed, json!([]));
    }

    #[tokio::test]
    async fn run_human_renders_empty_message() {
        let server = MockServer::start().await;
        arrange(&server, json!({"scheduled_freezes": []})).await;

        let mut cap = make_output(OutputMode::Human);
        let api_url = server.uri();
        run(
            ListOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                output_json: false,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let stdout = stdout_string(&cap);
        assert!(
            stdout.contains("No scheduled freezes found"),
            "got: {stdout:?}"
        );
    }

    #[tokio::test]
    async fn run_human_renders_table_with_columns() {
        let server = MockServer::start().await;
        arrange(&server, json!({"scheduled_freezes": [freeze_sample()]})).await;

        let mut cap = make_output(OutputMode::Human);
        let api_url = server.uri();
        run(
            ListOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                output_json: false,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let stdout = stdout_string(&cap);
        assert!(stdout.contains("Scheduled Freezes"), "got: {stdout}");
        assert!(stdout.contains("emergency-fix"), "got: {stdout}");
        assert!(
            stdout.contains("2026-01-01T10:00:00 (Europe/Paris)"),
            "got: {stdout}",
        );
        assert!(
            stdout.contains("2026-01-01T12:00:00 (Europe/Paris)"),
            "got: {stdout}",
        );
        // Matching + exclude conditions rendered together.
        assert!(stdout.contains("base=main"), "got: {stdout}");
        assert!(stdout.contains("(exclude: label=hotfix)"), "got: {stdout}");
    }

    #[tokio::test]
    async fn run_human_renders_dash_for_open_ended_freeze() {
        // `end == null` is a real API state — emergency freezes have
        // no scheduled lift. The table should show `"-"` (Python's
        // `_format_datetime(None, …) → "-"`), not the literal word
        // "null" or a parse error.
        let server = MockServer::start().await;
        arrange(
            &server,
            json!({
                "scheduled_freezes": [{
                    "id": "abc",
                    "reason": "emergency",
                    "start": "2026-01-01T10:00:00",
                    "end": null,
                    "timezone": "UTC",
                    "matching_conditions": [],
                    "exclude_conditions": [],
                }],
            }),
        )
        .await;

        let mut cap = make_output(OutputMode::Human);
        let api_url = server.uri();
        run(
            ListOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                output_json: false,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let stdout = stdout_string(&cap);
        // The end column should render as a bare `-` (we don't pin
        // the surrounding whitespace because the table's column
        // widths depend on the row content).
        assert!(stdout.contains(" - "), "got: {stdout}");
    }

    #[test]
    fn is_active_past_start_is_active() {
        let now = Utc::now();
        // 1h before now → active.
        let start = (now - chrono::Duration::hours(1))
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string();
        assert!(is_active(Some(&start), now));
    }

    #[test]
    fn is_active_future_start_is_scheduled() {
        let now = Utc::now();
        let start = (now + chrono::Duration::hours(1))
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string();
        assert!(!is_active(Some(&start), now));
    }

    #[test]
    fn is_active_missing_or_unparseable_is_scheduled() {
        let now = Utc::now();
        // Missing start — degrade to "scheduled" rather than
        // panicking. Matches Python's behavior: it would raise on
        // `fromisoformat(None)`, but our serde decoder lets the
        // field be absent, so the renderer falls through here.
        assert!(!is_active(None, now));
        assert!(!is_active(Some("not a date"), now));
    }

    #[test]
    fn format_conditions_matching_only() {
        let m = vec!["base=main".to_string(), "label=ready".to_string()];
        assert_eq!(format_conditions(&m, &[]), "base=main, label=ready");
    }

    #[test]
    fn format_conditions_with_exclude() {
        let m = vec!["base=main".to_string()];
        let e = vec!["label=hotfix".to_string()];
        assert_eq!(
            format_conditions(&m, &e),
            "base=main (exclude: label=hotfix)",
        );
    }

    #[test]
    fn format_conditions_exclude_only() {
        // Edge case the Python format produces a leading space-free
        // `(exclude: …)`. Mirror that.
        let m: Vec<String> = vec![];
        let e = vec!["label=hotfix".to_string()];
        assert_eq!(format_conditions(&m, &e), "(exclude: label=hotfix)");
    }
}
