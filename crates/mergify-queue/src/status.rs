//! `mergify queue status` — show merge queue status for a repository.
//!
//! `GET /v1/repos/<repo>/merge-queue/status[?branch=<branch>]`. Two
//! output modes:
//!
//! - `--json`: pretty-prints the raw API response as a single JSON
//!   document. The schema is Mergify's API contract, not this CLI's,
//!   so unknown fields are preserved (deserialize to
//!   `serde_json::Value`, emit verbatim).
//! - Human (default): a header, an optional pause indicator, the
//!   batch tree (grouped by scope when there is more than one), and
//!   the waiting-PR list. Status icons and relative times match the
//!   Python implementation.
//!
//! The command does not assume the response shape beyond the fields
//! it actively renders: every nested struct uses
//! `#[serde(default)] Option<…>` for fields the API has historically
//! treated as optional/nullable, so a missing field doesn't abort
//! deserialization.
//!
//! Exit codes:
//!
//! - `0` on a successful render (queue empty, paused, or active).
//! - Standard `CliError` exit codes on auth, API, or
//!   parse/serialization errors.

use std::collections::HashMap;
use std::collections::HashSet;
use std::io::Write;

use anstyle::AnsiColor;
use anstyle::Style;
use chrono::DateTime;
use chrono::Utc;
use indexmap::IndexMap;
use mergify_core::ApiFlavor;
use mergify_core::CliError;
use mergify_core::HttpClient;
use mergify_core::Output;
use mergify_core::auth;
use mergify_tui::Theme;
use mergify_tui::relative_time;
use mergify_tui::tree;
use serde::Deserialize;
use url::form_urlencoded;

pub struct StatusOptions<'a> {
    pub repository: Option<&'a str>,
    pub token: Option<&'a str>,
    pub api_url: Option<&'a str>,
    pub branch: Option<&'a str>,
    pub output_json: bool,
}

// All view structs use `#[serde(default)] Option<…>` for fields the
// API has historically treated as optional/nullable. The wire format
// is Mergify's API contract — we deserialize only the fields we
// render and accept everything else implicitly via the
// `serde_json::Value` passthrough used in JSON mode.
#[derive(Deserialize)]
struct StatusView {
    #[serde(default)]
    pause: Option<Pause>,
    #[serde(default)]
    batches: Vec<Batch>,
    #[serde(default)]
    waiting_pull_requests: Vec<PullRequest>,
}

#[derive(Deserialize)]
struct Pause {
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    paused_at: Option<String>,
}

#[derive(Deserialize)]
struct Batch {
    id: String,
    #[serde(default)]
    parent_ids: Vec<String>,
    #[serde(default)]
    scopes: Vec<String>,
    status: BatchStatus,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    estimated_merge_at: Option<String>,
    checks_summary: ChecksSummary,
    #[serde(default)]
    pull_requests: Vec<PullRequest>,
}

#[derive(Deserialize)]
struct BatchStatus {
    code: String,
}

#[derive(Deserialize)]
struct ChecksSummary {
    #[serde(default)]
    passed: u64,
    #[serde(default)]
    total: u64,
}

#[derive(Deserialize)]
struct PullRequest {
    number: u64,
    title: String,
    author: Author,
    #[serde(default)]
    queued_at: Option<String>,
    #[serde(default)]
    priority_alias: Option<String>,
    #[serde(default)]
    estimated_merge_at: Option<String>,
}

#[derive(Deserialize)]
struct Author {
    login: String,
}

/// Run the `queue status` command.
pub async fn run(opts: StatusOptions<'_>, output: &mut dyn Output) -> Result<(), CliError> {
    let repository = auth::resolve_repository(opts.repository)?;
    let token = auth::resolve_token(opts.token)?;
    let api_url = auth::resolve_api_url(opts.api_url)?;

    output.status(&format!("Fetching merge queue status for {repository}…"))?;

    let client = HttpClient::new(api_url, token, ApiFlavor::Mergify)?;
    let path = build_path(&repository, opts.branch);

    let raw: serde_json::Value = client.get(&path).await?;

    if opts.output_json {
        output.emit_json_value(&raw)?;
    } else {
        let view: StatusView = serde_json::from_value(raw)
            .map_err(|e| CliError::Generic(format!("decode merge queue status response: {e}")))?;
        emit_human(output, &repository, &view)?;
    }
    Ok(())
}

fn build_path(repository: &str, branch: Option<&str>) -> String {
    let mut path = format!("/v1/repos/{repository}/merge-queue/status");
    if let Some(branch) = branch {
        // form_urlencoded::byte_serialize handles spaces, unicode and
        // reserved characters. Unencoded slashes are tolerated by
        // most servers but encoding is the safe contract.
        let encoded: String = form_urlencoded::byte_serialize(branch.as_bytes()).collect();
        path.push_str("?branch=");
        path.push_str(&encoded);
    }
    path
}

fn emit_human(output: &mut dyn Output, repository: &str, view: &StatusView) -> std::io::Result<()> {
    let now = Utc::now();
    let theme = Theme::detect();
    output.emit(&(), &mut |w: &mut dyn Write| {
        writeln!(
            w,
            "{B}Merge Queue: {repository}{R}",
            B = theme.bold,
            R = theme.reset
        )?;
        writeln!(w)?;

        if let Some(pause) = &view.pause {
            print_pause(w, &theme, pause, now)?;
            writeln!(w)?;
        }

        if view.batches.is_empty() && view.waiting_pull_requests.is_empty() {
            writeln!(w, "{D}Queue is empty{R}", D = theme.dim, R = theme.reset)?;
            return Ok(());
        }

        if !view.batches.is_empty() {
            print_batches(w, &theme, &view.batches, now)?;
        }

        if !view.waiting_pull_requests.is_empty() {
            if !view.batches.is_empty() {
                writeln!(w)?;
            }
            print_waiting_prs(w, &theme, &view.waiting_pull_requests, now)?;
        }
        Ok(())
    })
}

/// Map a queue batch status code to a foreground color, honoring
/// the theme's enabled flag. Mirrors Python's `STATUS_STYLES`;
/// unknown codes render dim.
fn batch_status_style(theme: &Theme, code: &str) -> Style {
    if !theme.enabled {
        return Style::new();
    }
    match code {
        "running" => theme.fg(AnsiColor::Green),
        // Python rendered `merged` as `"dim green"` — bold off,
        // green on, dimmed. anstyle composes the same effect by
        // setting `dimmed()` on the green style.
        "merged" => theme.fg(AnsiColor::Green).dimmed(),
        "failed" => theme.fg(AnsiColor::Red),
        "bisecting"
        | "preparing"
        | "waiting_for_previous_batches"
        | "waiting_for_requeue"
        | "waiting_schedule" => theme.fg(AnsiColor::Yellow),
        "waiting_for_merge" | "frozen" => theme.fg(AnsiColor::Cyan),
        _ => theme.dim,
    }
}

fn print_pause(
    w: &mut dyn Write,
    theme: &Theme,
    pause: &Pause,
    now: DateTime<Utc>,
) -> std::io::Result<()> {
    let reason = pause.reason.as_deref().unwrap_or("");
    // Only the warning prefix gets the `warn` (bold yellow) style;
    // the quoted reason renders plain so it reads as content, not
    // as part of the warning text. Mirrors the Python rendering.
    write!(
        w,
        "{W}⚠ Queue is paused:{R} \"{reason}\"",
        W = theme.warn,
        R = theme.reset,
    )?;
    if let Some(ts) = &pause.paused_at {
        let rel = relative_time(ts, now, false);
        if !rel.is_empty() {
            write!(w, " {D}(since {rel}){R}", D = theme.dim, R = theme.reset)?;
        }
    }
    writeln!(w)
}

fn print_batches(
    w: &mut dyn Write,
    theme: &Theme,
    batches: &[Batch],
    now: DateTime<Utc>,
) -> std::io::Result<()> {
    let sorted = topological_sort(batches);
    let groups = group_by_scope(&sorted);
    let single_scope = groups.len() == 1;

    for (i, (scope, scope_batches)) in groups.iter().enumerate() {
        if i > 0 {
            writeln!(w)?;
        }
        let label = if single_scope {
            "Batches"
        } else {
            scope.as_str()
        };
        writeln!(w, "{B}{label}{R}", B = theme.bold, R = theme.reset)?;

        let last_batch_idx = scope_batches.len() - 1;
        for (bi, batch) in scope_batches.iter().enumerate() {
            let (branch, continuation) = tree::branch_chars(bi == last_batch_idx);
            print_batch_line(w, theme, branch, batch, now)?;
            print_batch_prs(w, theme, continuation, batch)?;
        }
    }
    Ok(())
}

fn print_batch_line(
    w: &mut dyn Write,
    theme: &Theme,
    branch: &str,
    batch: &Batch,
    now: DateTime<Utc>,
) -> std::io::Result<()> {
    let icon = status_icon(&batch.status.code);
    let icon_style = batch_status_style(theme, &batch.status.code);
    write!(
        w,
        "{branch}{S}{icon} {code}{R}",
        S = icon_style,
        code = batch.status.code,
        R = theme.reset,
    )?;
    if batch.checks_summary.total > 0 {
        write!(
            w,
            "  {D}checks {p}/{t}{R}",
            D = theme.dim,
            p = batch.checks_summary.passed,
            t = batch.checks_summary.total,
            R = theme.reset,
        )?;
    }
    if let Some(started) = &batch.started_at {
        let rel = relative_time(started, now, false);
        if !rel.is_empty() {
            write!(w, "  {D}{rel}{R}", D = theme.dim, R = theme.reset)?;
        }
    }
    if let Some(eta) = &batch.estimated_merge_at {
        let rel = relative_time(eta, now, true);
        if !rel.is_empty() {
            write!(w, "  {D}ETA {rel}{R}", D = theme.dim, R = theme.reset)?;
        }
    }
    writeln!(w)
}

fn print_batch_prs(
    w: &mut dyn Write,
    theme: &Theme,
    continuation: &str,
    batch: &Batch,
) -> std::io::Result<()> {
    if batch.pull_requests.is_empty() {
        return Ok(());
    }
    let last_pr_idx = batch.pull_requests.len() - 1;
    for (pi, pr) in batch.pull_requests.iter().enumerate() {
        let (pr_branch, _) = tree::branch_chars(pi == last_pr_idx);
        writeln!(
            w,
            "{continuation}{pr_branch}{N}#{num}{R} {title} {A}({author}){R}",
            N = theme.cyan,
            num = pr.number,
            title = pr.title,
            A = theme.dim,
            author = pr.author.login,
            R = theme.reset,
        )?;
    }
    Ok(())
}

fn print_waiting_prs(
    w: &mut dyn Write,
    theme: &Theme,
    prs: &[PullRequest],
    now: DateTime<Utc>,
) -> std::io::Result<()> {
    writeln!(w, "{B}Waiting{R}", B = theme.bold, R = theme.reset)?;
    for pr in prs {
        write!(
            w,
            "  {N}#{num}{R}  {title}  {A}{author}{R}",
            N = theme.cyan,
            num = pr.number,
            title = pr.title,
            A = theme.dim,
            author = pr.author.login,
            R = theme.reset,
        )?;
        if let Some(prio) = &pr.priority_alias {
            write!(w, "  {P}{prio}{R}", P = theme.magenta, R = theme.reset)?;
        }
        if let Some(queued_at) = &pr.queued_at {
            let rel = relative_time(queued_at, now, false);
            if !rel.is_empty() {
                write!(w, "  {D}queued {rel}{R}", D = theme.dim, R = theme.reset)?;
            }
        }
        if let Some(eta) = &pr.estimated_merge_at {
            let rel = relative_time(eta, now, true);
            if !rel.is_empty() {
                write!(w, "  {D}ETA {rel}{R}", D = theme.dim, R = theme.reset)?;
            }
        }
        writeln!(w)?;
    }
    Ok(())
}

/// Map a batch-status code to a compact Unicode icon. Same icons as
/// the Python implementation; unknown codes fall back to `?`.
fn status_icon(code: &str) -> &'static str {
    match code {
        "running" => "●",
        "bisecting" => "◑",
        "preparing" => "◌",
        "failed" => "✗",
        "merged" => "✓",
        "waiting_for_merge" => "◎",
        "waiting_for_previous_batches" | "waiting_for_batch" => "⏳",
        "waiting_for_requeue" => "↻",
        "waiting_schedule" => "⏰",
        "frozen" => "❄",
        _ => "?",
    }
}

/// Topological sort of batches by `parent_ids`. Roots come first,
/// children follow their parents — matches the Python
/// `_topological_sort`. Cycles are impossible by API contract, but
/// the `visited` set makes us tolerant of them anyway.
fn topological_sort(batches: &[Batch]) -> Vec<&Batch> {
    let id_to_batch: HashMap<&str, &Batch> = batches.iter().map(|b| (b.id.as_str(), b)).collect();
    let mut visited: HashSet<&str> = HashSet::new();
    let mut result: Vec<&Batch> = Vec::with_capacity(batches.len());

    for batch in batches {
        visit(batch.id.as_str(), &id_to_batch, &mut visited, &mut result);
    }
    result
}

fn visit<'a>(
    id: &'a str,
    id_to_batch: &HashMap<&'a str, &'a Batch>,
    visited: &mut HashSet<&'a str>,
    result: &mut Vec<&'a Batch>,
) {
    if !visited.insert(id) {
        return;
    }
    let Some(batch) = id_to_batch.get(id) else {
        return;
    };
    for parent in &batch.parent_ids {
        visit(parent.as_str(), id_to_batch, visited, result);
    }
    result.push(batch);
}

/// Group batches by scope, preserving insertion order for the
/// scopes (matches Python dict iteration). A batch with no scopes
/// is grouped under `"default"` to match the Python fallback. A
/// batch with multiple scopes appears in every group it claims —
/// the Python implementation does the same so users see each batch
/// in every scope it affects.
fn group_by_scope<'a>(batches: &[&'a Batch]) -> IndexMap<String, Vec<&'a Batch>> {
    let mut groups: IndexMap<String, Vec<&Batch>> = IndexMap::new();
    for batch in batches {
        let scopes: Vec<String> = if batch.scopes.is_empty() {
            vec!["default".to_string()]
        } else {
            batch.scopes.clone()
        };
        for scope in scopes {
            groups.entry(scope).or_default().push(batch);
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use mergify_test_support::Captured;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::header;
    use wiremock::matchers::method;
    use wiremock::matchers::path;
    use wiremock::matchers::query_param;

    use super::*;

    #[test]
    fn build_path_no_branch() {
        assert_eq!(
            build_path("owner/repo", None),
            "/v1/repos/owner/repo/merge-queue/status",
        );
    }

    #[test]
    fn build_path_with_branch() {
        assert_eq!(
            build_path("owner/repo", Some("main")),
            "/v1/repos/owner/repo/merge-queue/status?branch=main",
        );
    }

    #[test]
    fn build_path_url_encodes_branch() {
        // Slashes and unicode in branch names must survive a round
        // trip through the URL — `feature/foo` is common, and
        // browser-pasted names occasionally include UTF-8.
        let path = build_path("owner/repo", Some("feature/foo bar"));
        assert!(path.ends_with("?branch=feature%2Ffoo+bar"), "got {path}");
    }

    // `relative_time` lives in `mergify-tui::time` and is exercised
    // there; we re-export it via `mergify_tui::relative_time` and
    // don't re-test it here.

    #[test]
    fn topological_sort_orders_parents_before_children() {
        // Construct three batches, child references parent. Even if
        // the input is in reverse order, the sort must put the
        // parent first.
        let batches = vec![
            sample_batch("c", &["b"]),
            sample_batch("b", &["a"]),
            sample_batch("a", &[]),
        ];
        let sorted = topological_sort(&batches);
        let ids: Vec<&str> = sorted.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn topological_sort_handles_missing_parent_ids() {
        // When `parent_ids` references an id that isn't in the
        // batches list (the API has dropped it for some reason),
        // the sort skips it instead of panicking.
        let batches = [sample_batch("only", &["nonexistent"])];
        let sorted = topological_sort(&batches);
        assert_eq!(sorted.len(), 1);
        assert_eq!(sorted[0].id, "only");
    }

    #[test]
    fn group_by_scope_default_when_empty_scopes() {
        let batches = [sample_batch("a", &[])];
        let refs: Vec<&Batch> = batches.iter().collect();
        let groups = group_by_scope(&refs);
        assert_eq!(groups.len(), 1);
        assert!(groups.contains_key("default"));
    }

    #[test]
    fn group_by_scope_assigns_to_each_listed_scope() {
        // Matches Python: a multi-scope batch appears under each
        // scope's group, not just the first.
        let mut b = sample_batch("a", &[]);
        b.scopes = vec!["foo".to_string(), "bar".to_string()];
        let batches = [b];
        let refs: Vec<&Batch> = batches.iter().collect();
        let groups = group_by_scope(&refs);
        assert_eq!(groups.len(), 2);
        assert!(groups.contains_key("foo"));
        assert!(groups.contains_key("bar"));
    }

    #[test]
    fn status_icon_known_codes() {
        assert_eq!(status_icon("running"), "●");
        assert_eq!(status_icon("merged"), "✓");
        assert_eq!(status_icon("failed"), "✗");
        // Two pairs that share an icon vs. a different icon —
        // mirrors the Python `STATUS_STYLES` table, so a future
        // table edit can't silently swap glyphs without updating
        // this test.
        assert_eq!(status_icon("waiting_for_previous_batches"), "⏳");
        assert_eq!(status_icon("waiting_for_batch"), "⏳");
        assert_eq!(status_icon("waiting_for_requeue"), "↻");
    }

    #[test]
    fn status_icon_unknown_falls_back() {
        assert_eq!(status_icon("brand-new-status"), "?");
    }

    fn sample_batch(id: &str, parents: &[&str]) -> Batch {
        Batch {
            id: id.to_string(),
            parent_ids: parents.iter().copied().map(String::from).collect(),
            scopes: Vec::new(),
            status: BatchStatus {
                code: "running".to_string(),
            },
            started_at: None,
            estimated_merge_at: None,
            checks_summary: ChecksSummary {
                passed: 0,
                total: 0,
            },
            pull_requests: Vec::new(),
        }
    }

    #[tokio::test]
    async fn run_json_passes_response_through_verbatim() {
        // JSON mode is a passthrough — every field the server sends,
        // including ones we don't render, must survive intact.
        // `extra_field` here proves we don't reshape on the way out.
        let server = MockServer::start().await;
        let response = serde_json::json!({
            "batches": [],
            "waiting_pull_requests": [],
            "scope_queues": {"default": []},
            "pause": null,
            "extra_field": "preserved",
        });
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/merge-queue/status"))
            .and(header("Authorization", "Bearer t"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response.clone()))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = Captured::human();
        let api_url = server.uri();
        run(
            StatusOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                branch: None,
                output_json: true,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let stdout = cap.stdout();
        let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(parsed, response);
    }

    #[tokio::test]
    async fn run_human_renders_paused_queue() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/merge-queue/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "batches": [],
                "waiting_pull_requests": [],
                "scope_queues": {},
                "pause": {"reason": "deploy freeze", "paused_at": "2026-01-01T00:00:00Z"},
            })))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = Captured::human();
        let api_url = server.uri();
        run(
            StatusOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                branch: None,
                output_json: false,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let stdout = cap.stdout();
        assert!(stdout.contains("Merge Queue: owner/repo"), "got {stdout}");
        assert!(stdout.contains("Queue is paused"), "got {stdout}");
        assert!(stdout.contains("deploy freeze"), "got {stdout}");
        assert!(stdout.contains("Queue is empty"), "got {stdout}");
    }

    #[tokio::test]
    async fn run_human_renders_empty_queue() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/merge-queue/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "batches": [],
                "waiting_pull_requests": [],
                "scope_queues": {},
                "pause": null,
            })))
            .mount(&server)
            .await;

        let mut cap = Captured::human();
        let api_url = server.uri();
        run(
            StatusOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                branch: None,
                output_json: false,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let stdout = cap.stdout();
        assert!(stdout.contains("Queue is empty"), "got {stdout}");
    }

    #[tokio::test]
    async fn run_human_renders_batches_and_waiting_prs() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/merge-queue/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "batches": [{
                    "id": "b1",
                    "name": "batch-1",
                    "status": {"code": "running"},
                    "checks_summary": {"passed": 3, "total": 5},
                    "started_at": "2026-01-01T00:00:00Z",
                    "estimated_merge_at": "2026-01-01T01:00:00Z",
                    "pull_requests": [
                        {
                            "number": 42,
                            "title": "Add feature foo",
                            "url": "https://example.test/42",
                            "author": {"id": 1, "login": "alice"},
                            "queued_at": "2026-01-01T00:00:00Z",
                            "priority_alias": "default",
                            "priority_rule_name": "default",
                            "labels": [],
                            "scopes": [],
                        },
                    ],
                    "parent_ids": [],
                }],
                "waiting_pull_requests": [
                    {
                        "number": 43,
                        "title": "Update deps",
                        "url": "https://example.test/43",
                        "author": {"id": 2, "login": "bob"},
                        "queued_at": "2026-01-01T00:00:00Z",
                        "priority_alias": "high",
                        "priority_rule_name": "high",
                        "labels": [],
                        "scopes": [],
                    },
                ],
                "scope_queues": {},
                "pause": null,
            })))
            .mount(&server)
            .await;

        let mut cap = Captured::human();
        let api_url = server.uri();
        run(
            StatusOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                branch: None,
                output_json: false,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let stdout = cap.stdout();
        assert!(stdout.contains("Batches"), "got {stdout}");
        assert!(stdout.contains("running"), "got {stdout}");
        assert!(stdout.contains("checks 3/5"), "got {stdout}");
        assert!(
            stdout.contains("#42 Add feature foo (alice)"),
            "got {stdout}"
        );
        assert!(stdout.contains("Waiting"), "got {stdout}");
        assert!(stdout.contains("#43"), "got {stdout}");
        assert!(stdout.contains("Update deps"), "got {stdout}");
        assert!(stdout.contains("bob"), "got {stdout}");
        assert!(stdout.contains("high"), "got {stdout}");
    }

    #[tokio::test]
    async fn run_human_groups_batches_by_scope_when_multiple() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/merge-queue/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "batches": [
                    {
                        "id": "b1",
                        "status": {"code": "running"},
                        "checks_summary": {"passed": 0, "total": 0},
                        "pull_requests": [],
                        "scopes": ["frontend"],
                        "parent_ids": [],
                    },
                    {
                        "id": "b2",
                        "status": {"code": "preparing"},
                        "checks_summary": {"passed": 0, "total": 0},
                        "pull_requests": [],
                        "scopes": ["backend"],
                        "parent_ids": [],
                    },
                ],
                "waiting_pull_requests": [],
                "scope_queues": {},
                "pause": null,
            })))
            .mount(&server)
            .await;

        let mut cap = Captured::human();
        let api_url = server.uri();
        run(
            StatusOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                branch: None,
                output_json: false,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let stdout = cap.stdout();
        // Two scopes → each labelled by its own name (no
        // generic "Batches" header).
        assert!(stdout.contains("frontend"), "got {stdout}");
        assert!(stdout.contains("backend"), "got {stdout}");
        assert!(!stdout.contains("\nBatches\n"), "got {stdout}");
    }

    #[tokio::test]
    async fn run_passes_branch_query_param() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/merge-queue/status"))
            .and(query_param("branch", "main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "batches": [],
                "waiting_pull_requests": [],
                "scope_queues": {},
                "pause": null,
            })))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = Captured::human();
        let api_url = server.uri();
        run(
            StatusOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                branch: Some("main"),
                output_json: false,
            },
            &mut cap.output,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn run_tolerates_missing_optional_fields() {
        // The API has historically dropped optional fields entirely
        // rather than serializing them as null. Deserialization
        // must accept that — the response below has neither
        // `pause` nor any of the per-batch optional timestamps.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/merge-queue/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "batches": [{
                    "id": "b1",
                    "status": {"code": "running"},
                    "checks_summary": {"passed": 0, "total": 0},
                    "pull_requests": [],
                }],
                "waiting_pull_requests": [],
                "scope_queues": {},
            })))
            .mount(&server)
            .await;

        let mut cap = Captured::human();
        let api_url = server.uri();
        run(
            StatusOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                branch: None,
                output_json: false,
            },
            &mut cap.output,
        )
        .await
        .unwrap();
    }
}
