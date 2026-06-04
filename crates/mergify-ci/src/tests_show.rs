//! `mergify tests show` — resolve test identities by name and fetch
//! full health/metrics details for each match.
//!
//! Exit code `2` is intentionally skipped: the CLI-wide contract
//! reserves it for clap argument errors.

use std::io::{self, Write};
use std::sync::Arc;

use chrono::DateTime;
use chrono::Utc;
use mergify_core::ApiFlavor;
use mergify_core::CliError;
use mergify_core::ExitCode;
use mergify_core::HttpClient;
use mergify_core::Output;
use mergify_core::auth;
use serde::Deserialize;
use serde::Serialize;

use crate::detector::resolve_repository;
use crate::detector::split_owner_repo;

const DETAILS_FANOUT: usize = 5;

pub struct TestsShowOptions<'a> {
    /// Explicit `--repository owner/repo`, or `None` to detect it
    /// from the CI environment.
    pub repository: Option<&'a str>,
    pub test_names: &'a [String],
    pub token: Option<&'a str>,
    pub api_url: Option<&'a str>,
    pub pipeline_name: &'a [String],
    pub pipeline_name_exclude: &'a [String],
    pub job_name: &'a [String],
    pub job_name_exclude: &'a [String],
    pub per_page: Option<u32>,
}

/// Run the command and return the exit code that reflects the
/// aggregate test health.
pub async fn run(
    opts: TestsShowOptions<'_>,
    output: &mut dyn Output,
) -> Result<ExitCode, CliError> {
    let repository = resolve_repository(opts.repository)?;
    let (owner, repo) = split_owner_repo(&repository)?;
    let token = auth::resolve_token(opts.token)?;
    let api_url = auth::resolve_api_url(opts.api_url)?;
    let client = Arc::new(HttpClient::new(api_url, token, ApiFlavor::Mergify)?);

    let search_path = format!("/v1/ci/{owner}/repositories/{repo}/search/tests");
    let identities = search(&client, &search_path, &opts).await?;

    if identities.is_empty() {
        let quoted = opts
            .test_names
            .iter()
            .map(|n| format!("'{n}'"))
            .collect::<Vec<_>>()
            .join(", ");
        output.status(&format!("no tests matched {quoted}"))?;
        let payload = TestsShowPayload { tests: vec![] };
        output.emit(&payload, &mut |_| Ok(()))?;
        return Ok(ExitCode::Success);
    }

    let details = fetch_all_details(client.clone(), owner, repo, &identities).await?;

    let payload = TestsShowPayload::new(identities, details);
    let exit_code = aggregate_exit_code(payload.tests.iter().map(|t| t.details.health_status));

    output.emit(&payload, &mut |w| render_human(w, &payload))?;

    Ok(exit_code)
}

async fn search(
    client: &HttpClient,
    path: &str,
    opts: &TestsShowOptions<'_>,
) -> Result<Vec<TestSearchResult>, CliError> {
    let per_page = opts.per_page.map(|n| n.to_string());
    let query = build_search_query(opts, per_page.as_deref());
    let response: SearchTestsResponse = client.get_with_query(path, &query).await?;
    Ok(response.tests)
}

/// Optional filters with no values are omitted so the server falls
/// back to its own defaults.
fn build_search_query<'a>(
    opts: &'a TestsShowOptions<'a>,
    per_page: Option<&'a str>,
) -> Vec<(&'a str, &'a str)> {
    let mut query: Vec<(&str, &str)> = Vec::new();
    for name in opts.test_names {
        query.push(("test_name", name));
    }
    for name in opts.pipeline_name {
        query.push(("pipeline_name", name));
    }
    for name in opts.pipeline_name_exclude {
        query.push(("pipeline_name_exclude", name));
    }
    for name in opts.job_name {
        query.push(("job_name", name));
    }
    for name in opts.job_name_exclude {
        query.push(("job_name_exclude", name));
    }
    if let Some(per_page) = per_page {
        query.push(("per_page", per_page));
    }
    query
}

/// Returned details follow the input order of `identities` regardless
/// of completion order — callers rely on it for stable output.
async fn fetch_all_details(
    client: Arc<HttpClient>,
    owner: &str,
    repo: &str,
    identities: &[TestSearchResult],
) -> Result<Vec<TestDetails>, CliError> {
    let mut set: tokio::task::JoinSet<(usize, Result<TestDetails, CliError>)> =
        tokio::task::JoinSet::new();
    let mut results: Vec<Option<TestDetails>> = (0..identities.len()).map(|_| None).collect();
    let mut next = 0usize;

    let spawn_at = |set: &mut tokio::task::JoinSet<_>, index: usize| {
        let client = client.clone();
        let path = format!(
            "/v1/ci/{owner}/repositories/{repo}/tests/{}",
            identities[index].test_id,
        );
        set.spawn(async move { (index, client.get::<TestDetails>(&path).await) });
    };

    while next < identities.len() && next < DETAILS_FANOUT {
        spawn_at(&mut set, next);
        next += 1;
    }
    while let Some(joined) = set.join_next().await {
        // Tasks are never cancelled, so a `JoinError` can only mean a
        // panic — propagate it verbatim instead of wrapping.
        let (index, result) =
            joined.unwrap_or_else(|err| std::panic::resume_unwind(err.into_panic()));
        results[index] = Some(result?);
        if next < identities.len() {
            spawn_at(&mut set, next);
            next += 1;
        }
    }

    Ok(results
        .into_iter()
        .map(|slot| slot.expect("slot filled by JoinSet loop"))
        .collect())
}

fn aggregate_exit_code(statuses: impl IntoIterator<Item = HealthStatus>) -> ExitCode {
    let mut max = ExitCode::Success;
    for status in statuses {
        match status {
            HealthStatus::Broken => return ExitCode::MergifyApiError,
            HealthStatus::Flaky if max == ExitCode::Success => max = ExitCode::GenericError,
            _ => {}
        }
    }
    max
}

#[derive(Deserialize)]
struct SearchTestsResponse {
    tests: Vec<TestSearchResult>,
}

#[derive(Deserialize)]
struct TestSearchResult {
    test_id: String,
    pipeline_name: String,
    job_name: String,
}

#[derive(Deserialize, Serialize)]
struct TestDetails {
    repository: String,
    test_name: String,
    test_id: String,
    health_status: HealthStatus,
    last_conclusion: LastConclusion,
    failure_ratio: f64,
    flakiness_ratio: f64,
    success_ratio: f64,
    flaky_detection_enabled: bool,
    first_failure_at: Option<DateTime<Utc>>,
    first_failure_commit: Option<String>,
    first_failure_pull: Option<TestDetailsPull>,
    last_failure_at: Option<DateTime<Utc>>,
    last_success_at: Option<DateTime<Utc>>,
    test_framework: Option<String>,
    test_framework_version: Option<String>,
    test_programming_language: Option<String>,
    test_filepath: Option<String>,
    test_function_name: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct TestDetailsPull {
    id: u64,
    number: u64,
    title: String,
    user: TestDetailsUser,
}

#[derive(Deserialize, Serialize)]
struct TestDetailsUser {
    id: u64,
    login: String,
}

#[derive(Copy, Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum HealthStatus {
    Healthy,
    Flaky,
    Broken,
    /// Captures any future server-side enum value so a single
    /// unrecognized status does not abort the whole batch.
    #[serde(other, alias = "unknown")]
    Unknown,
}

#[derive(Copy, Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum LastConclusion {
    Passed,
    Failed,
    Skipped,
    /// Reserved for future API additions; rendered as `—`.
    #[serde(other)]
    Unknown,
}

#[derive(Serialize)]
struct TestsShowPayload {
    tests: Vec<TestRow>,
}

#[derive(Serialize)]
struct TestRow {
    pipeline_name: String,
    job_name: String,
    #[serde(flatten)]
    details: TestDetails,
}

impl TestsShowPayload {
    fn new(identities: Vec<TestSearchResult>, details: Vec<TestDetails>) -> Self {
        let tests = identities
            .into_iter()
            .zip(details)
            .map(|(identity, details)| TestRow {
                pipeline_name: identity.pipeline_name,
                job_name: identity.job_name,
                details,
            })
            .collect();
        Self { tests }
    }
}

fn render_human(w: &mut dyn Write, payload: &TestsShowPayload) -> io::Result<()> {
    for (index, row) in payload.tests.iter().enumerate() {
        if index > 0 {
            writeln!(w)?;
        }
        render_one(w, row)?;
    }
    Ok(())
}

fn render_one(w: &mut dyn Write, row: &TestRow) -> io::Result<()> {
    let details = &row.details;
    writeln!(w, "{}", details.test_name)?;
    writeln!(w, "  test_id:        {}", details.test_id)?;
    writeln!(
        w,
        "  pipeline:       {} › job: {}",
        row.pipeline_name, row.job_name,
    )?;
    if let Some(language) = &details.test_programming_language {
        writeln!(w, "  language:       {language}")?;
    }
    if let Some(framework) = &details.test_framework {
        match &details.test_framework_version {
            Some(version) => writeln!(w, "  framework:      {framework} {version}")?,
            None => writeln!(w, "  framework:      {framework}")?,
        }
    }
    if let Some(filepath) = &details.test_filepath {
        writeln!(w, "  file:           {filepath}")?;
    }
    if let Some(function) = &details.test_function_name {
        writeln!(w, "  function:       {function}")?;
    }
    writeln!(
        w,
        "  health:         {}",
        health_label(details.health_status)
    )?;
    writeln!(
        w,
        "  last result:    {}",
        conclusion_label(details.last_conclusion)
    )?;
    writeln!(w, "  success ratio:  {:.1}%", details.success_ratio * 100.0)?;
    writeln!(w, "  failure ratio:  {:.1}%", details.failure_ratio * 100.0)?;
    if details.flaky_detection_enabled {
        writeln!(
            w,
            "  flakiness:      {:.1}%",
            details.flakiness_ratio * 100.0
        )?;
    }
    if let Some(ts) = details.last_success_at {
        writeln!(w, "  last success:   {}", format_timestamp(ts))?;
    }
    if let Some(ts) = details.last_failure_at {
        writeln!(w, "  last failure:   {}", format_timestamp(ts))?;
    }
    if let Some(ts) = details.first_failure_at {
        writeln!(w, "  first failure:  {}", format_timestamp(ts))?;
        let commit = details.first_failure_commit.as_deref();
        if let Some(pull) = &details.first_failure_pull {
            let prefix = commit
                .map(|c| format!("commit {} in ", short_sha(c)))
                .unwrap_or_default();
            writeln!(
                w,
                "                  {prefix}#{} \"{}\"",
                pull.number, pull.title,
            )?;
            writeln!(w, "                  by {}", pull.user.login)?;
        } else if let Some(short) = commit.map(short_sha) {
            writeln!(w, "                  commit {short}")?;
        }
    }
    Ok(())
}

fn health_label(status: HealthStatus) -> &'static str {
    match status {
        HealthStatus::Healthy => "● healthy",
        HealthStatus::Flaky => "● flaky",
        HealthStatus::Broken => "✗ broken",
        HealthStatus::Unknown => "— unknown",
    }
}

fn conclusion_label(conclusion: LastConclusion) -> &'static str {
    match conclusion {
        LastConclusion::Passed => "✓ passed",
        LastConclusion::Failed => "✗ failed",
        LastConclusion::Skipped => "○ skipped",
        LastConclusion::Unknown => "— unknown",
    }
}

fn format_timestamp(ts: DateTime<Utc>) -> String {
    ts.format("%Y-%m-%d %H:%M UTC").to_string()
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::Mutex;

    use mergify_core::OutputMode;
    use mergify_core::StdioOutput;
    use serde_json::json;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path as path_matcher;

    use super::*;
    use crate::testing::with_ci_env_async;

    type SharedBytes = Arc<Mutex<Vec<u8>>>;

    struct SharedWriter(SharedBytes);

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct Captured {
        output: StdioOutput,
        stdout: SharedBytes,
        stderr: SharedBytes,
    }

    fn captured(mode: OutputMode) -> Captured {
        let stdout: SharedBytes = Arc::new(Mutex::new(Vec::new()));
        let stderr: SharedBytes = Arc::new(Mutex::new(Vec::new()));
        let output = StdioOutput::with_sinks(
            mode,
            SharedWriter(Arc::clone(&stdout)),
            SharedWriter(Arc::clone(&stderr)),
        );
        Captured {
            output,
            stdout,
            stderr,
        }
    }

    fn read(b: &SharedBytes) -> String {
        String::from_utf8(b.lock().unwrap().clone()).unwrap()
    }

    /// Mount the search endpoint. The fixture body usually contains
    /// just `{"tests": [...]}`; the real API also returns `size` and
    /// `per_page` next to `tests`, but the CLI ignores those, so
    /// callers don't need to pass them.
    async fn mount_search(server: &MockServer, body: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path_matcher("/v1/ci/owner/repositories/repo/search/tests"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    async fn mount_details(server: &MockServer, id: &str, response: ResponseTemplate) {
        Mock::given(method("GET"))
            .and(path_matcher(format!(
                "/v1/ci/owner/repositories/repo/tests/{id}"
            )))
            .respond_with(response)
            .mount(server)
            .await;
    }

    fn details_template(id: &str, name: &str, health: &str, conclusion: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(details_json(id, name, health, conclusion))
    }

    fn details_json(
        test_id: &str,
        test_name: &str,
        health: &str,
        conclusion: &str,
    ) -> serde_json::Value {
        json!({
            "repository": "monorepo",
            "test_name": test_name,
            "test_id": test_id,
            "health_status": health,
            "last_conclusion": conclusion,
            "failure_ratio": 0.08,
            "flakiness_ratio": 0.0,
            "success_ratio": 0.92,
            "flaky_detection_enabled": false,
            "first_failure_at": null,
            "first_failure_commit": null,
            "first_failure_pull": null,
            "last_failure_at": null,
            "last_success_at": null,
            "test_framework": null,
            "test_framework_version": null,
            "test_programming_language": null,
            "test_filepath": null,
            "test_function_name": null,
        })
    }

    fn test_id(n: usize) -> String {
        format!("00000000-0000-5000-8000-00000000000{n}")
    }

    fn options<'a>(api_url: &'a str, names: &'a [String]) -> TestsShowOptions<'a> {
        TestsShowOptions {
            repository: Some("owner/repo"),
            test_names: names,
            token: Some("test-token"),
            api_url: Some(api_url),
            pipeline_name: &[],
            pipeline_name_exclude: &[],
            job_name: &[],
            job_name_exclude: &[],
            per_page: None,
        }
    }

    #[tokio::test]
    async fn empty_search_emits_empty_tests_and_returns_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v1/ci/owner/repositories/repo/search/tests"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"tests": []})))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = captured(OutputMode::Json);
        let api_url = server.uri();
        let names = vec!["ghost".to_string()];
        let exit = run(options(&api_url, &names), &mut cap.output)
            .await
            .unwrap();

        assert_eq!(exit, ExitCode::Success);
        let stdout = read(&cap.stdout);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&stdout).unwrap(),
            json!({"tests": []})
        );
    }

    #[tokio::test]
    async fn detects_repository_from_ci_env() {
        // With no `--repository`, the command resolves the repository
        // from the CI environment — here GitHub Actions'
        // `GITHUB_REPOSITORY` — and queries that repository's endpoint.
        with_ci_env_async(
            &[
                ("GITHUB_ACTIONS", Some("true")),
                ("GITHUB_REPOSITORY", Some("owner/repo")),
            ],
            async {
                let server = MockServer::start().await;
                mount_search(&server, json!({"tests": []})).await;

                let mut cap = captured(OutputMode::Json);
                let api_url = server.uri();
                let names = vec!["ghost".to_string()];
                let opts = TestsShowOptions {
                    repository: None,
                    test_names: &names,
                    token: Some("test-token"),
                    api_url: Some(&api_url),
                    pipeline_name: &[],
                    pipeline_name_exclude: &[],
                    job_name: &[],
                    job_name_exclude: &[],
                    per_page: None,
                };
                let exit = run(opts, &mut cap.output).await.unwrap();
                assert_eq!(exit, ExitCode::Success);
            },
        )
        .await;
    }

    #[tokio::test]
    async fn empty_search_in_human_mode_writes_no_match_to_stderr() {
        let server = MockServer::start().await;
        mount_search(&server, json!({"tests": []})).await;

        let mut cap = captured(OutputMode::Human);
        let api_url = server.uri();
        let names = vec!["ghost".to_string()];
        run(options(&api_url, &names), &mut cap.output)
            .await
            .unwrap();

        assert!(read(&cap.stderr).contains("no tests matched 'ghost'"));
        assert_eq!(read(&cap.stdout), "");
    }

    #[tokio::test]
    async fn single_match_fetches_details_and_renders_in_human_mode() {
        let server = MockServer::start().await;
        let id = test_id(1);
        mount_search(
            &server,
            json!({
                "tests": [{
                    "test_id": id,
                    "test_name": "test_login",
                    "pipeline_name": "ci",
                    "job_name": "unit",
                }]
            }),
        )
        .await;
        mount_details(
            &server,
            &id,
            details_template(&id, "test_login", "healthy", "passed"),
        )
        .await;

        let mut cap = captured(OutputMode::Human);
        let api_url = server.uri();
        let names = vec!["test_login".to_string()];
        let exit = run(options(&api_url, &names), &mut cap.output)
            .await
            .unwrap();

        assert_eq!(exit, ExitCode::Success);
        let out = read(&cap.stdout);
        assert!(out.contains("test_login"), "missing test name in {out:?}");
        assert!(out.contains("● healthy"), "missing health line in {out:?}");
        assert!(out.contains("✓ passed"), "missing conclusion in {out:?}");
        assert!(
            out.contains("success ratio:  92.0%"),
            "missing ratio in {out:?}"
        );
        assert!(
            !out.contains("flakiness"),
            "flakiness line must be hidden when flaky_detection_enabled=false, got {out:?}"
        );
        assert!(
            !out.contains("disabled"),
            "no `disabled` placeholder allowed, got {out:?}"
        );
    }

    #[tokio::test]
    async fn batch_preserves_search_order_even_when_responses_arrive_out_of_order() {
        let server = MockServer::start().await;
        let id_a = test_id(1);
        let id_b = test_id(2);
        let id_c = test_id(3);

        mount_search(
            &server,
            json!({
                "tests": [
                    {"test_id": id_a, "test_name": "alpha", "pipeline_name": "ci", "job_name": "j"},
                    {"test_id": id_b, "test_name": "bravo", "pipeline_name": "ci", "job_name": "j"},
                    {"test_id": id_c, "test_name": "charlie", "pipeline_name": "ci", "job_name": "j"},
                ]
            }),
        )
        .await;

        let delayed = |id: &str, name: &str, ms: u64| {
            details_template(id, name, "healthy", "passed")
                .set_delay(std::time::Duration::from_millis(ms))
        };
        mount_details(&server, &id_a, delayed(&id_a, "alpha", 80)).await;
        mount_details(&server, &id_b, delayed(&id_b, "bravo", 0)).await;
        mount_details(&server, &id_c, delayed(&id_c, "charlie", 40)).await;

        let mut cap = captured(OutputMode::Json);
        let api_url = server.uri();
        let names = vec!["*".to_string()];
        run(options(&api_url, &names), &mut cap.output)
            .await
            .unwrap();

        let stdout = read(&cap.stdout);
        let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        let names: Vec<&str> = value["tests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["test_name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["alpha", "bravo", "charlie"]);
    }

    #[tokio::test]
    async fn exit_code_promotes_for_flaky_then_broken() {
        let server = MockServer::start().await;
        let id_a = test_id(1);
        let id_b = test_id(2);
        let id_c = test_id(3);

        mount_search(
            &server,
            json!({
                "tests": [
                    {"test_id": id_a, "test_name": "a", "pipeline_name": "ci", "job_name": "j"},
                    {"test_id": id_b, "test_name": "b", "pipeline_name": "ci", "job_name": "j"},
                    {"test_id": id_c, "test_name": "c", "pipeline_name": "ci", "job_name": "j"},
                ]
            }),
        )
        .await;
        mount_details(
            &server,
            &id_a,
            details_template(&id_a, "a", "healthy", "passed"),
        )
        .await;
        mount_details(
            &server,
            &id_b,
            details_template(&id_b, "b", "flaky", "passed"),
        )
        .await;
        mount_details(
            &server,
            &id_c,
            details_template(&id_c, "c", "broken", "failed"),
        )
        .await;

        let mut cap = captured(OutputMode::Json);
        let api_url = server.uri();
        let names = vec!["*".to_string()];
        let exit = run(options(&api_url, &names), &mut cap.output)
            .await
            .unwrap();
        assert_eq!(
            exit,
            ExitCode::MergifyApiError,
            "broken must promote past flaky"
        );
    }

    #[tokio::test]
    async fn exit_code_is_generic_error_when_only_flaky() {
        let server = MockServer::start().await;
        let id = test_id(1);
        mount_search(
            &server,
            json!({
                "tests": [
                    {"test_id": id, "test_name": "a", "pipeline_name": "ci", "job_name": "j"},
                ]
            }),
        )
        .await;
        mount_details(&server, &id, details_template(&id, "a", "flaky", "passed")).await;

        let mut cap = captured(OutputMode::Json);
        let api_url = server.uri();
        let names = vec!["a".to_string()];
        let exit = run(options(&api_url, &names), &mut cap.output)
            .await
            .unwrap();
        assert_eq!(exit, ExitCode::GenericError);
    }

    #[tokio::test]
    async fn unknown_health_status_does_not_promote_severity() {
        let server = MockServer::start().await;
        let id = test_id(1);
        mount_search(
            &server,
            json!({
                "tests": [
                    {"test_id": id, "test_name": "a", "pipeline_name": "ci", "job_name": "j"},
                ]
            }),
        )
        .await;
        mount_details(
            &server,
            &id,
            details_template(&id, "a", "future_value_we_dont_know", "passed"),
        )
        .await;

        let mut cap = captured(OutputMode::Json);
        let api_url = server.uri();
        let names = vec!["a".to_string()];
        let exit = run(options(&api_url, &names), &mut cap.output)
            .await
            .unwrap();
        assert_eq!(
            exit,
            ExitCode::Success,
            "unknown enum must not promote severity"
        );
        let mut cap = captured(OutputMode::Human);
        run(options(&api_url, &names), &mut cap.output)
            .await
            .unwrap();
        assert!(read(&cap.stdout).contains("— unknown"));
    }

    #[tokio::test]
    async fn flaky_detection_enabled_true_renders_flakiness_line() {
        let server = MockServer::start().await;
        let id = test_id(1);
        mount_search(
            &server,
            json!({
                "tests": [
                    {"test_id": id, "test_name": "a", "pipeline_name": "ci", "job_name": "j"},
                ]
            }),
        )
        .await;
        let mut details = details_json(&id, "a", "flaky", "passed");
        details["flaky_detection_enabled"] = json!(true);
        details["flakiness_ratio"] = json!(0.12);
        mount_details(
            &server,
            &id,
            ResponseTemplate::new(200).set_body_json(details),
        )
        .await;

        let mut cap = captured(OutputMode::Human);
        let api_url = server.uri();
        let names = vec!["a".to_string()];
        run(options(&api_url, &names), &mut cap.output)
            .await
            .unwrap();
        let out = read(&cap.stdout);
        assert!(out.contains("flakiness:      12.0%"), "got {out:?}");
    }

    #[tokio::test]
    async fn optional_metadata_lines_rendered_when_present() {
        let server = MockServer::start().await;
        let id = test_id(1);
        mount_search(
            &server,
            json!({
                "tests": [
                    {"test_id": id, "test_name": "test_login", "pipeline_name": "ci", "job_name": "j"},
                ]
            }),
        )
        .await;
        let mut details = details_json(&id, "test_login", "healthy", "passed");
        details["test_framework"] = json!("pytest");
        details["test_framework_version"] = json!("8.3.2");
        details["test_programming_language"] = json!("python");
        details["test_filepath"] = json!("tests/auth.py");
        details["test_function_name"] = json!("test_login");
        details["last_success_at"] = json!("2026-05-06T08:00:00Z");
        details["first_failure_at"] = json!("2026-04-07T07:52:28Z");
        details["first_failure_commit"] = json!("9ee5a5183b0640220d009982ab62e336d3f64d0f");
        details["first_failure_pull"] = json!({
            "id": 3_496_538_490_u64,
            "number": 28834,
            "title": "chore(deps): update click",
            "user": {"id": 29_139_614, "login": "renovate[bot]"},
        });
        mount_details(
            &server,
            &id,
            ResponseTemplate::new(200).set_body_json(details),
        )
        .await;

        let mut cap = captured(OutputMode::Human);
        let api_url = server.uri();
        let names = vec!["test_login".to_string()];
        run(options(&api_url, &names), &mut cap.output)
            .await
            .unwrap();
        let out = read(&cap.stdout);
        for needle in [
            "language:       python",
            "framework:      pytest 8.3.2",
            "file:           tests/auth.py",
            "function:       test_login",
            "last success:   2026-05-06 08:00 UTC",
            "first failure:  2026-04-07 07:52 UTC",
            "commit 9ee5a51 in #28834",
            "by renovate[bot]",
        ] {
            assert!(
                out.contains(needle),
                "expected {needle:?} in output:\n{out}"
            );
        }
    }

    #[tokio::test]
    async fn search_query_includes_names_and_filters_and_omits_absent_ones() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v1/ci/owner/repositories/repo/search/tests"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"tests": []})))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = captured(OutputMode::Json);
        let api_url = server.uri();
        let names = vec!["alpha".to_string(), "*beta*".to_string()];
        let pipelines = vec!["e2e".to_string()];
        let job_excludes = vec!["lint".to_string()];

        let opts = TestsShowOptions {
            repository: Some("owner/repo"),
            test_names: &names,
            token: Some("t"),
            api_url: Some(&api_url),
            pipeline_name: &pipelines,
            pipeline_name_exclude: &[],
            job_name: &[],
            job_name_exclude: &job_excludes,
            per_page: Some(20),
        };
        run(opts, &mut cap.output).await.unwrap();

        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1);
        let pairs: Vec<(String, String)> = received[0]
            .url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("test_name".into(), "alpha".into()),
                ("test_name".into(), "*beta*".into()),
                ("pipeline_name".into(), "e2e".into()),
                ("job_name_exclude".into(), "lint".into()),
                ("per_page".into(), "20".into()),
            ],
        );
    }

    #[tokio::test]
    async fn details_endpoint_4xx_surfaces_as_error() {
        let server = MockServer::start().await;
        let id_a = test_id(1);
        let id_b = test_id(2);
        mount_search(
            &server,
            json!({
                "tests": [
                    {"test_id": id_a, "test_name": "a", "pipeline_name": "ci", "job_name": "j"},
                    {"test_id": id_b, "test_name": "b", "pipeline_name": "ci", "job_name": "j"},
                ]
            }),
        )
        .await;
        mount_details(
            &server,
            &id_a,
            ResponseTemplate::new(404).set_body_string("not found"),
        )
        .await;
        mount_details(
            &server,
            &id_b,
            details_template(&id_b, "b", "healthy", "passed"),
        )
        .await;

        let mut cap = captured(OutputMode::Json);
        let api_url = server.uri();
        let names = vec!["*".to_string()];
        let err = run(options(&api_url, &names), &mut cap.output)
            .await
            .unwrap_err();
        assert!(matches!(err, CliError::MergifyApi(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn invalid_repository_format_is_a_configuration_error() {
        let mut cap = captured(OutputMode::Human);
        let names = vec!["a".to_string()];
        let opts = TestsShowOptions {
            repository: Some("not-a-slash-pair"),
            test_names: &names,
            token: Some("t"),
            api_url: Some("https://example.invalid"),
            pipeline_name: &[],
            pipeline_name_exclude: &[],
            job_name: &[],
            job_name_exclude: &[],
            per_page: None,
        };
        let err = run(opts, &mut cap.output).await.unwrap_err();
        assert!(matches!(err, CliError::Configuration(_)), "got {err:?}");
    }
}
