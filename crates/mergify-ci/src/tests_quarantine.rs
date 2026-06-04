//! `mergify tests quarantines add` / `remove` / `get` / `list` — add a
//! test to, remove it from, fetch one from, or list the CI Insights
//! quarantine through the public quarantine API.
//!
//! `add` addresses a test by its fully qualified name. `remove`
//! accepts either the quarantine id (deleted directly, as the DELETE
//! endpoint is keyed by it) or a test name. The quarantine resource
//! has the composite primary key `(repository, test_name)`, so a name
//! resolves to at most one quarantine per repository — `remove` and
//! `get` rely on this when they look the name up in the list endpoint.
//! `list` renders every record; `get` filters that same list to the
//! single match (the API exposes no single-quarantine GET).

use std::io::{self, Write};

use chrono::DateTime;
use chrono::Utc;
use mergify_core::ApiFlavor;
use mergify_core::CliError;
use mergify_core::DeleteOutcome;
use mergify_core::ExitCode;
use mergify_core::HttpClient;
use mergify_core::Output;
use mergify_core::auth;
use serde::Deserialize;
use serde::Serialize;

use crate::detector::resolve_repository;
use crate::detector::split_owner_repo;

pub struct QuarantineOptions<'a> {
    /// Explicit `--repository owner/repo`, or `None` to detect it
    /// from the CI environment.
    pub repository: Option<&'a str>,
    pub test_name: &'a str,
    pub reason: &'a str,
    pub branch: Option<&'a str>,
    pub token: Option<&'a str>,
    pub api_url: Option<&'a str>,
}

pub struct UnquarantineOptions<'a> {
    /// Explicit `--repository owner/repo`, or `None` to detect it
    /// from the CI environment.
    pub repository: Option<&'a str>,
    /// A test name or a quarantine id. A UUID-shaped value is taken as
    /// the quarantine id; anything else is treated as a test name.
    pub name_or_id: &'a str,
    pub token: Option<&'a str>,
    pub api_url: Option<&'a str>,
}

pub struct QuarantinedOptions<'a> {
    /// Explicit `--repository owner/repo`, or `None` to detect it
    /// from the CI environment.
    pub repository: Option<&'a str>,
    pub token: Option<&'a str>,
    pub api_url: Option<&'a str>,
}

pub struct GetOptions<'a> {
    /// Explicit `--repository owner/repo`, or `None` to detect it
    /// from the CI environment.
    pub repository: Option<&'a str>,
    /// A test name or a quarantine id. A UUID-shaped value is matched
    /// against the quarantine id; anything else against the test name.
    pub name_or_id: &'a str,
    pub token: Option<&'a str>,
    pub api_url: Option<&'a str>,
}

/// Quarantine a test: POST the new quarantine and report the id the
/// server assigned it.
pub async fn quarantine(
    opts: QuarantineOptions<'_>,
    output: &mut dyn Output,
) -> Result<ExitCode, CliError> {
    let repository = resolve_repository(opts.repository)?;
    let (owner, repo) = split_owner_repo(&repository)?;
    let token = auth::resolve_token(opts.token)?;
    let api_url = auth::resolve_api_url(opts.api_url)?;
    let client = HttpClient::new(api_url, token, ApiFlavor::Mergify)?;

    let path = format!("/v1/ci/{owner}/repositories/{repo}/quarantines");
    let request = AddQuarantineRequest {
        test_name: opts.test_name,
        reason: opts.reason,
        branch: opts.branch,
    };
    let response: AddQuarantineResponse = client.post(&path, &request).await?;

    let result = QuarantineResult {
        id: response.id,
        test_name: opts.test_name.to_string(),
        reason: opts.reason.to_string(),
        branch: opts.branch.map(str::to_string),
    };
    output.emit(&result, &mut |w| render_quarantined(w, &result))?;
    Ok(ExitCode::Success)
}

/// Unquarantine a test, addressed either by quarantine id (a
/// UUID-shaped value) or by test name. A test name is resolved to its
/// quarantine id through the list endpoint; an id is deleted directly.
pub async fn unquarantine(
    opts: UnquarantineOptions<'_>,
    output: &mut dyn Output,
) -> Result<ExitCode, CliError> {
    let repository = resolve_repository(opts.repository)?;
    let (owner, repo) = split_owner_repo(&repository)?;
    let token = auth::resolve_token(opts.token)?;
    let api_url = auth::resolve_api_url(opts.api_url)?;
    let client = HttpClient::new(api_url, token, ApiFlavor::Mergify)?;

    let target = resolve_target(&client, owner, repo, opts.name_or_id).await?;

    let delete_path = format!(
        "/v1/ci/{owner}/repositories/{repo}/quarantines/{}",
        target.id
    );
    if client.delete_if_exists(&delete_path).await? == DeleteOutcome::NotFound {
        // For a name we just listed, this means a concurrent removal
        // raced us; for an id passed straight in, it never existed.
        // Either way the quarantine is gone — report it as not found.
        return Err(not_found(opts.name_or_id));
    }

    let result = UnquarantineResult {
        id: target.id,
        test_name: target.test_name,
    };
    output.emit(&result, &mut |w| render_unquarantined(w, &result))?;
    Ok(ExitCode::Success)
}

/// List every quarantine recorded for the repository, one block per
/// record. Returns `Success` even when the quarantine is empty — an
/// empty list is a normal state, not an error (mirrors `freeze list`).
/// Auth, request, and decode failures still propagate as errors.
pub async fn quarantined(
    opts: QuarantinedOptions<'_>,
    output: &mut dyn Output,
) -> Result<ExitCode, CliError> {
    let quarantined_tests = fetch_quarantines(opts.repository, opts.token, opts.api_url).await?;
    let payload = QuarantinedPayload { quarantined_tests };
    output.emit(&payload, &mut |w| render_quarantined_list(w, &payload))?;
    Ok(ExitCode::Success)
}

/// Fetch a single quarantine, addressed either by quarantine id (a
/// UUID-shaped value) or by test name, and render the full record.
/// Both keys are resolved client-side against the list endpoint, the
/// same source `quarantined` and `unquarantine` read — the API has no
/// single-quarantine GET. Errors when no quarantine matches.
pub async fn get(opts: GetOptions<'_>, output: &mut dyn Output) -> Result<ExitCode, CliError> {
    let quarantines = fetch_quarantines(opts.repository, opts.token, opts.api_url).await?;

    // Match by id when the argument is UUID-shaped, else by name —
    // decided once rather than per row.
    let by_id = looks_like_uuid(opts.name_or_id);
    let test = quarantines
        .into_iter()
        .find(|quarantine| {
            if by_id {
                quarantine.id.eq_ignore_ascii_case(opts.name_or_id)
            } else {
                quarantine.test_name == opts.name_or_id
            }
        })
        .ok_or_else(|| not_found(opts.name_or_id))?;

    output.emit(&test, &mut |w| render_quarantined_one(w, &test))?;
    Ok(ExitCode::Success)
}

/// Fetch every quarantine recorded for the repository. Shared by
/// `quarantined` (renders all) and `get` (filters to one); omitting
/// `per_page` returns the full list in a single response.
async fn fetch_quarantines(
    repository: Option<&str>,
    token: Option<&str>,
    api_url: Option<&str>,
) -> Result<Vec<QuarantinedTest>, CliError> {
    let repository = resolve_repository(repository)?;
    let (owner, repo) = split_owner_repo(&repository)?;
    let token = auth::resolve_token(token)?;
    let api_url = auth::resolve_api_url(api_url)?;
    let client = HttpClient::new(api_url, token, ApiFlavor::Mergify)?;

    let path = format!("/v1/ci/{owner}/repositories/{repo}/quarantines");
    let list: QuarantineList<QuarantinedTest> = client.get(&path).await?;
    Ok(list.quarantined_tests)
}

/// The quarantine targeted by `unquarantine`, with the test name
/// included only when we learned it (i.e. resolved by name).
struct UnquarantineTarget {
    id: String,
    test_name: Option<String>,
}

/// Map the user's `name_or_id` to a quarantine. A UUID-shaped value is
/// taken as the quarantine id verbatim (no lookup); anything else is a
/// test name, resolved through the list endpoint where each item
/// carries the id the DELETE endpoint needs.
async fn resolve_target(
    client: &HttpClient,
    owner: &str,
    repo: &str,
    name_or_id: &str,
) -> Result<UnquarantineTarget, CliError> {
    if looks_like_uuid(name_or_id) {
        return Ok(UnquarantineTarget {
            // Lowercase the canonical form so the path segment is one
            // the API parses regardless of the input's casing.
            id: name_or_id.to_lowercase(),
            test_name: None,
        });
    }

    // Omitting `per_page` returns the full list in a single response.
    let list_path = format!("/v1/ci/{owner}/repositories/{repo}/quarantines");
    let list: QuarantineList<QuarantineListItem> = client.get(&list_path).await?;
    list.quarantined_tests
        .into_iter()
        .find(|quarantine| quarantine.test_name == name_or_id)
        .map(|found| UnquarantineTarget {
            id: found.id,
            test_name: Some(found.test_name),
        })
        .ok_or_else(|| not_found(name_or_id))
}

/// The quarantine couldn't be found. Mirrors `queue show`'s "PR not in
/// the merge queue" — a resolvable-but-absent resource maps to a
/// Mergify API error (exit code 6).
fn not_found(name_or_id: &str) -> CliError {
    CliError::MergifyApi(format!("'{name_or_id}' is not quarantined"))
}

/// Whether `value` has the canonical hyphenated UUID shape
/// (`8-4-4-4-12` hex digits). Used to tell a quarantine id apart from
/// a test name without pulling in a UUID-parsing dependency; test
/// names realistically never take this shape.
fn looks_like_uuid(value: &str) -> bool {
    let groups = [8, 4, 4, 4, 12];
    let mut parts = value.split('-');
    for length in groups {
        match parts.next() {
            Some(part) if part.len() == length && part.bytes().all(|b| b.is_ascii_hexdigit()) => {}
            _ => return false,
        }
    }
    parts.next().is_none()
}

#[derive(Serialize)]
struct AddQuarantineRequest<'a> {
    test_name: &'a str,
    reason: &'a str,
    // Omitted when absent so the server applies its "all branches"
    // default, matching the `branch: str | None = None` payload field.
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<&'a str>,
}

#[derive(Deserialize)]
struct AddQuarantineResponse {
    id: String,
}

/// The list endpoint's envelope. Generic over the row type so callers
/// pull only the fields they need: `unquarantine` resolves a name to an
/// id with the minimal [`QuarantineListItem`], while `quarantined`
/// reads the full [`QuarantinedTest`] for display.
#[derive(Deserialize)]
struct QuarantineList<T> {
    quarantined_tests: Vec<T>,
}

#[derive(Deserialize)]
struct QuarantineListItem {
    id: String,
    test_name: String,
}

/// A full quarantine record as listed by `quarantined`. Every field is
/// re-serialized in `--json` mode, so the fields the human block omits
/// (e.g. `created_at`) still reach machine consumers.
#[derive(Deserialize, Serialize)]
struct QuarantinedTest {
    id: String,
    test_name: String,
    reason: String,
    // Null means the quarantine applies to every branch.
    branch: Option<String>,
    created_at: DateTime<Utc>,
    // How the quarantine was created — `manual` by a user, `auto` by
    // flaky detection. Kept as the raw string (the API models it as a
    // free-form string with a `manual` default, not a closed enum) so
    // `--json` stays faithful and an omitted value still deserializes.
    #[serde(default = "default_source")]
    source: String,
    is_recovered: bool,
}

fn default_source() -> String {
    "manual".to_string()
}

#[derive(Serialize)]
struct QuarantineResult {
    id: String,
    test_name: String,
    reason: String,
    // Always present (null when unscoped) so JSON consumers get a
    // stable schema.
    branch: Option<String>,
}

#[derive(Serialize)]
struct UnquarantineResult {
    id: String,
    // Present when the quarantine was addressed by name (or otherwise
    // resolved); null when deleted directly by id, where the name is
    // never fetched.
    test_name: Option<String>,
}

/// The `quarantined` payload. Wraps the rows under `quarantined_tests`
/// so `--json` emits `{"quarantined_tests": [...]}`, mirroring the API
/// envelope.
#[derive(Serialize)]
struct QuarantinedPayload {
    quarantined_tests: Vec<QuarantinedTest>,
}

fn render_quarantined(w: &mut dyn Write, result: &QuarantineResult) -> io::Result<()> {
    write!(w, "✓ Quarantined '{}'", result.test_name)?;
    if let Some(branch) = &result.branch {
        write!(w, " on branch '{branch}'")?;
    }
    writeln!(w, " (id: {}).", result.id)
}

fn render_unquarantined(w: &mut dyn Write, result: &UnquarantineResult) -> io::Result<()> {
    match &result.test_name {
        Some(test_name) => writeln!(w, "✓ Unquarantined '{test_name}' (id: {}).", result.id),
        None => writeln!(w, "✓ Unquarantined quarantine {}.", result.id),
    }
}

/// Render each quarantine as an indented block followed by a count, or
/// a single line when nothing is quarantined. The test name gets its
/// own line — never a table cell — so a long name is never wrapped
/// mid-name. Mirrors `tests show`'s detail layout in this crate.
fn render_quarantined_list(w: &mut dyn Write, payload: &QuarantinedPayload) -> io::Result<()> {
    let tests = &payload.quarantined_tests;
    if tests.is_empty() {
        return writeln!(w, "No quarantined tests found.");
    }

    for (index, test) in tests.iter().enumerate() {
        if index > 0 {
            writeln!(w)?;
        }
        render_quarantined_one(w, test)?;
    }

    writeln!(w)?;
    let n = tests.len();
    writeln!(
        w,
        "{n} quarantined test{plural}",
        plural = if n == 1 { "" } else { "s" },
    )
}

/// One record: the test name as a header, then its id (the value
/// `quarantines remove` accepts) and metadata indented under it. A
/// null branch shows as `*`, the "all branches" marker `add`'s human
/// output already uses.
fn render_quarantined_one(w: &mut dyn Write, test: &QuarantinedTest) -> io::Result<()> {
    writeln!(w, "{}", test.test_name)?;
    writeln!(w, "  id:         {}", test.id)?;
    writeln!(w, "  branch:     {}", test.branch.as_deref().unwrap_or("*"))?;
    writeln!(w, "  source:     {}", test.source)?;
    writeln!(
        w,
        "  recovered:  {}",
        if test.is_recovered { "yes" } else { "no" },
    )?;
    writeln!(w, "  reason:     {}", test.reason)
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::Arc;
    use std::sync::Mutex;

    use mergify_core::OutputMode;
    use mergify_core::StdioOutput;
    use serde_json::json;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::body_json;
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
    }

    fn captured(mode: OutputMode) -> Captured {
        // These commands only `emit` (stdout); they never call
        // `status`, so stderr is routed to a throwaway sink.
        let stdout: SharedBytes = Arc::new(Mutex::new(Vec::new()));
        let output = StdioOutput::with_sinks(mode, SharedWriter(Arc::clone(&stdout)), io::sink());
        Captured { output, stdout }
    }

    fn read(b: &SharedBytes) -> String {
        String::from_utf8(b.lock().unwrap().clone()).unwrap()
    }

    const QUARANTINES_PATH: &str = "/v1/ci/owner/repositories/repo/quarantines";

    fn quarantine_options<'a>(
        api_url: &'a str,
        test_name: &'a str,
        reason: &'a str,
        branch: Option<&'a str>,
    ) -> QuarantineOptions<'a> {
        QuarantineOptions {
            repository: Some("owner/repo"),
            test_name,
            reason,
            branch,
            token: Some("test-token"),
            api_url: Some(api_url),
        }
    }

    fn unquarantine_options<'a>(api_url: &'a str, name_or_id: &'a str) -> UnquarantineOptions<'a> {
        UnquarantineOptions {
            repository: Some("owner/repo"),
            name_or_id,
            token: Some("test-token"),
            api_url: Some(api_url),
        }
    }

    #[tokio::test]
    async fn quarantine_posts_payload_and_emits_id_in_json() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_matcher(QUARANTINES_PATH))
            .and(body_json(json!({
                "test_name": "test_login",
                "reason": "flaky on CI",
                "branch": "main",
            })))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(json!({"id": "quarantine-uuid-1"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = captured(OutputMode::Json);
        let api_url = server.uri();
        let exit = quarantine(
            quarantine_options(&api_url, "test_login", "flaky on CI", Some("main")),
            &mut cap.output,
        )
        .await
        .unwrap();

        assert_eq!(exit, ExitCode::Success);
        let value: serde_json::Value = serde_json::from_str(&read(&cap.stdout)).unwrap();
        assert_eq!(
            value,
            json!({
                "id": "quarantine-uuid-1",
                "test_name": "test_login",
                "reason": "flaky on CI",
                "branch": "main",
            })
        );
    }

    #[tokio::test]
    async fn quarantine_without_branch_omits_it_from_payload() {
        let server = MockServer::start().await;
        // `body_json` is an exact match, so this asserts `branch` is
        // absent (not sent as null) when no branch is given.
        Mock::given(method("POST"))
            .and(path_matcher(QUARANTINES_PATH))
            .and(body_json(json!({
                "test_name": "test_login",
                "reason": "broken",
            })))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(json!({"id": "quarantine-uuid-2"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = captured(OutputMode::Human);
        let api_url = server.uri();
        quarantine(
            quarantine_options(&api_url, "test_login", "broken", None),
            &mut cap.output,
        )
        .await
        .unwrap();

        let out = read(&cap.stdout);
        assert!(out.contains("✓ Quarantined 'test_login'"), "got {out:?}");
        assert!(out.contains("quarantine-uuid-2"), "got {out:?}");
        assert!(
            !out.contains("on branch"),
            "no branch line expected, got {out:?}"
        );
    }

    #[tokio::test]
    async fn quarantine_surfaces_already_quarantined_as_mergify_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_matcher(QUARANTINES_PATH))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_string("Test 'test_login' is already quarantined"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = captured(OutputMode::Json);
        let api_url = server.uri();
        let err = quarantine(
            quarantine_options(&api_url, "test_login", "flaky", None),
            &mut cap.output,
        )
        .await
        .unwrap_err();

        assert!(matches!(err, CliError::MergifyApi(_)), "got {err:?}");
        assert!(err.to_string().contains("already quarantined"));
    }

    #[tokio::test]
    async fn unquarantine_resolves_id_by_name_then_deletes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher(QUARANTINES_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "quarantined_tests": [
                    {"id": "id-other", "test_name": "test_logout"},
                    {"id": "id-target", "test_name": "test_login"},
                ],
                "size": 2,
                "per_page": null,
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path_matcher(format!("{QUARANTINES_PATH}/id-target")))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = captured(OutputMode::Json);
        let api_url = server.uri();
        let exit = unquarantine(
            unquarantine_options(&api_url, "test_login"),
            &mut cap.output,
        )
        .await
        .unwrap();

        assert_eq!(exit, ExitCode::Success);
        let value: serde_json::Value = serde_json::from_str(&read(&cap.stdout)).unwrap();
        assert_eq!(value, json!({"id": "id-target", "test_name": "test_login"}));
    }

    #[tokio::test]
    async fn unquarantine_unknown_test_is_a_mergify_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher(QUARANTINES_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "quarantined_tests": [{"id": "id-other", "test_name": "test_logout"}],
                "size": 1,
                "per_page": null,
            })))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = captured(OutputMode::Human);
        let api_url = server.uri();
        let err = unquarantine(
            unquarantine_options(&api_url, "test_login"),
            &mut cap.output,
        )
        .await
        .unwrap_err();

        assert!(matches!(err, CliError::MergifyApi(_)), "got {err:?}");
        assert_eq!(err.exit_code(), ExitCode::MergifyApiError);
        assert!(err.to_string().contains("is not quarantined"), "got {err}");
        // No DELETE is attempted, so stdout stays empty.
        assert_eq!(read(&cap.stdout), "");
    }

    #[tokio::test]
    async fn unquarantine_delete_racing_to_404_is_reported_as_not_quarantined() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher(QUARANTINES_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "quarantined_tests": [{"id": "id-target", "test_name": "test_login"}],
                "size": 1,
                "per_page": null,
            })))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path_matcher(format!("{QUARANTINES_PATH}/id-target")))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let mut cap = captured(OutputMode::Human);
        let api_url = server.uri();
        let err = unquarantine(
            unquarantine_options(&api_url, "test_login"),
            &mut cap.output,
        )
        .await
        .unwrap_err();

        assert!(matches!(err, CliError::MergifyApi(_)), "got {err:?}");
        assert!(err.to_string().contains("is not quarantined"), "got {err}");
    }

    #[tokio::test]
    async fn quarantine_invalid_repository_format_is_a_configuration_error() {
        let mut cap = captured(OutputMode::Human);
        let opts = QuarantineOptions {
            repository: Some("not-a-slash-pair"),
            test_name: "test_login",
            reason: "flaky",
            branch: None,
            token: Some("t"),
            api_url: Some("https://example.invalid"),
        };
        let err = quarantine(opts, &mut cap.output).await.unwrap_err();
        assert!(matches!(err, CliError::Configuration(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn unquarantine_invalid_repository_format_is_a_configuration_error() {
        let mut cap = captured(OutputMode::Human);
        let opts = UnquarantineOptions {
            repository: Some("not-a-slash-pair"),
            name_or_id: "test_login",
            token: Some("t"),
            api_url: Some("https://example.invalid"),
        };
        let err = unquarantine(opts, &mut cap.output).await.unwrap_err();
        assert!(matches!(err, CliError::Configuration(_)), "got {err:?}");
    }

    // A canonical UUID used to exercise the "address by quarantine id"
    // path. Its lowercase hyphenated form is what the DELETE path must
    // carry.
    const QUARANTINE_ID: &str = "12345678-1234-5678-1234-567812345678";

    #[tokio::test]
    async fn unquarantine_by_id_deletes_directly_without_listing() {
        let server = MockServer::start().await;
        // The list endpoint must not be hit when an id is given.
        Mock::given(method("GET"))
            .and(path_matcher(QUARANTINES_PATH))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path_matcher(format!("{QUARANTINES_PATH}/{QUARANTINE_ID}")))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = captured(OutputMode::Json);
        let api_url = server.uri();
        let exit = unquarantine(
            unquarantine_options(&api_url, QUARANTINE_ID),
            &mut cap.output,
        )
        .await
        .unwrap();

        assert_eq!(exit, ExitCode::Success);
        // `test_name` is null: addressing by id never fetches the name.
        let value: serde_json::Value = serde_json::from_str(&read(&cap.stdout)).unwrap();
        assert_eq!(value, json!({"id": QUARANTINE_ID, "test_name": null}));
    }

    #[tokio::test]
    async fn unquarantine_by_id_normalizes_uppercase_to_canonical_path() {
        let server = MockServer::start().await;
        // DELETE must target the lowercase canonical form even though
        // the user passed the id uppercased.
        Mock::given(method("DELETE"))
            .and(path_matcher(format!("{QUARANTINES_PATH}/{QUARANTINE_ID}")))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = captured(OutputMode::Human);
        let api_url = server.uri();
        let uppercased = QUARANTINE_ID.to_uppercase();
        unquarantine(unquarantine_options(&api_url, &uppercased), &mut cap.output)
            .await
            .unwrap();

        let out = read(&cap.stdout);
        assert!(
            out.contains(&format!("quarantine {QUARANTINE_ID}")),
            "got {out:?}"
        );
    }

    #[tokio::test]
    async fn unquarantine_by_unknown_id_404_is_a_mergify_error() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path_matcher(format!("{QUARANTINES_PATH}/{QUARANTINE_ID}")))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let mut cap = captured(OutputMode::Json);
        let api_url = server.uri();
        let err = unquarantine(
            unquarantine_options(&api_url, QUARANTINE_ID),
            &mut cap.output,
        )
        .await
        .unwrap_err();

        assert!(matches!(err, CliError::MergifyApi(_)), "got {err:?}");
        assert!(err.to_string().contains("is not quarantined"), "got {err}");
        assert_eq!(read(&cap.stdout), "");
    }

    fn quarantined_options(api_url: &str) -> QuarantinedOptions<'_> {
        QuarantinedOptions {
            repository: Some("owner/repo"),
            token: Some("test-token"),
            api_url: Some(api_url),
        }
    }

    async fn mount_quarantine_list(server: &MockServer, body: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path_matcher(QUARANTINES_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn quarantined_detects_repository_from_ci_env() {
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
                mount_quarantine_list(
                    &server,
                    json!({"quarantined_tests": [], "size": 0, "per_page": null}),
                )
                .await;

                let mut cap = captured(OutputMode::Human);
                let api_url = server.uri();
                let opts = QuarantinedOptions {
                    repository: None,
                    token: Some("test-token"),
                    api_url: Some(&api_url),
                };
                let exit = quarantined(opts, &mut cap.output).await.unwrap();
                assert_eq!(exit, ExitCode::Success);
            },
        )
        .await;
    }

    #[tokio::test]
    async fn quarantined_renders_each_record_as_a_block() {
        let server = MockServer::start().await;
        mount_quarantine_list(
            &server,
            json!({
                "quarantined_tests": [
                    {
                        "id": "id-1",
                        "test_name": "test_login",
                        "reason": "flaky — MRGFY-1234",
                        "branch": null,
                        "created_at": "2026-01-01T10:00:00Z",
                        "source": "manual",
                        "is_recovered": false,
                    },
                    {
                        "id": "id-2",
                        "test_name": "test_logout",
                        "reason": "consistently failing",
                        "branch": "release/*",
                        "created_at": "2026-02-01T12:00:00Z",
                        "source": "auto",
                        "is_recovered": true,
                    },
                ],
                "size": 2,
                "per_page": null,
            }),
        )
        .await;

        let mut cap = captured(OutputMode::Human);
        let api_url = server.uri();
        let exit = quarantined(quarantined_options(&api_url), &mut cap.output)
            .await
            .unwrap();

        assert_eq!(exit, ExitCode::Success);
        let out = read(&cap.stdout);
        // Each test name sits on its own line (a block header), with the
        // quarantine id shown so the row is actionable for `unquarantine`.
        assert!(out.contains("test_login\n"), "got {out:?}");
        assert!(out.contains("id:         id-1"), "id missing, got {out:?}");
        assert!(out.contains("id:         id-2"), "id missing, got {out:?}");
        // A null branch renders as the all-branches marker.
        assert!(
            out.contains("branch:     *"),
            "null branch should show '*', got {out:?}"
        );
        assert!(out.contains("source:     manual"), "got {out:?}");
        assert!(out.contains("branch:     release/*"), "got {out:?}");
        assert!(out.contains("source:     auto"), "got {out:?}");
        assert!(out.contains("recovered:  yes"), "got {out:?}");
        assert!(out.contains("recovered:  no"), "got {out:?}");
        assert!(
            out.contains("reason:     flaky — MRGFY-1234"),
            "got {out:?}"
        );
        assert!(
            out.contains("2 quarantined tests"),
            "count missing, got {out:?}"
        );
    }

    #[tokio::test]
    async fn quarantined_empty_list_renders_message() {
        let server = MockServer::start().await;
        mount_quarantine_list(
            &server,
            json!({"quarantined_tests": [], "size": 0, "per_page": null}),
        )
        .await;

        let mut cap = captured(OutputMode::Human);
        let api_url = server.uri();
        quarantined(quarantined_options(&api_url), &mut cap.output)
            .await
            .unwrap();

        let out = read(&cap.stdout);
        assert!(out.contains("No quarantined tests found"), "got {out:?}");
    }

    #[tokio::test]
    async fn quarantined_json_emits_every_field() {
        let server = MockServer::start().await;
        mount_quarantine_list(
            &server,
            json!({
                "quarantined_tests": [{
                    "id": "id-1",
                    "test_name": "test_login",
                    "reason": "flaky",
                    "branch": null,
                    "created_at": "2026-01-01T10:00:00Z",
                    "source": "auto",
                    "is_recovered": true,
                }],
                "size": 1,
                "per_page": null,
            }),
        )
        .await;

        let mut cap = captured(OutputMode::Json);
        let api_url = server.uri();
        quarantined(quarantined_options(&api_url), &mut cap.output)
            .await
            .unwrap();

        let value: serde_json::Value = serde_json::from_str(&read(&cap.stdout)).unwrap();
        assert_eq!(
            value,
            json!({
                "quarantined_tests": [{
                    "id": "id-1",
                    "test_name": "test_login",
                    "reason": "flaky",
                    "branch": null,
                    "created_at": "2026-01-01T10:00:00Z",
                    "source": "auto",
                    "is_recovered": true,
                }],
            })
        );
    }

    #[tokio::test]
    async fn quarantined_preserves_unmodeled_source_verbatim() {
        // The API models `source` as a free-form string; a value beyond
        // manual/auto must reach `--json` unchanged, not normalized.
        let server = MockServer::start().await;
        mount_quarantine_list(
            &server,
            json!({
                "quarantined_tests": [{
                    "id": "id-1",
                    "test_name": "test_login",
                    "reason": "flaky",
                    "branch": null,
                    "created_at": "2026-01-01T10:00:00Z",
                    "source": "some_future_source",
                    "is_recovered": false,
                }],
                "size": 1,
                "per_page": null,
            }),
        )
        .await;

        let mut cap = captured(OutputMode::Json);
        let api_url = server.uri();
        quarantined(quarantined_options(&api_url), &mut cap.output)
            .await
            .unwrap();

        let value: serde_json::Value = serde_json::from_str(&read(&cap.stdout)).unwrap();
        assert_eq!(
            value["quarantined_tests"][0]["source"], "some_future_source",
            "unmodeled source must round-trip verbatim, got {value}"
        );
    }

    #[tokio::test]
    async fn quarantined_omitted_source_defaults_to_manual() {
        // The schema marks `source` optional with a `manual` default, so
        // a record that omits it must still deserialize — not abort the
        // whole list — and render as `manual`.
        let server = MockServer::start().await;
        mount_quarantine_list(
            &server,
            json!({
                "quarantined_tests": [{
                    "id": "id-1",
                    "test_name": "test_login",
                    "reason": "flaky",
                    "branch": null,
                    "created_at": "2026-01-01T10:00:00Z",
                    "is_recovered": false,
                }],
                "size": 1,
                "per_page": null,
            }),
        )
        .await;

        let mut cap = captured(OutputMode::Human);
        let api_url = server.uri();
        quarantined(quarantined_options(&api_url), &mut cap.output)
            .await
            .unwrap();

        let out = read(&cap.stdout);
        assert!(out.contains("source:     manual"), "got {out:?}");
    }

    #[tokio::test]
    async fn quarantined_invalid_repository_format_is_a_configuration_error() {
        let mut cap = captured(OutputMode::Human);
        let opts = QuarantinedOptions {
            repository: Some("not-a-slash-pair"),
            token: Some("t"),
            api_url: Some("https://example.invalid"),
        };
        let err = quarantined(opts, &mut cap.output).await.unwrap_err();
        assert!(matches!(err, CliError::Configuration(_)), "got {err:?}");
    }

    fn get_options<'a>(api_url: &'a str, name_or_id: &'a str) -> GetOptions<'a> {
        GetOptions {
            repository: Some("owner/repo"),
            name_or_id,
            token: Some("test-token"),
            api_url: Some(api_url),
        }
    }

    fn one_quarantine_list(id: &str, test_name: &str) -> serde_json::Value {
        json!({
            "quarantined_tests": [{
                "id": id,
                "test_name": test_name,
                "reason": "flaky — MRGFY-1234",
                "branch": null,
                "created_at": "2026-01-01T10:00:00Z",
                "source": "manual",
                "is_recovered": false,
            }],
            "size": 1,
            "per_page": null,
        })
    }

    #[tokio::test]
    async fn get_by_name_renders_the_matching_record() {
        let server = MockServer::start().await;
        mount_quarantine_list(&server, one_quarantine_list("id-1", "test_login")).await;

        let mut cap = captured(OutputMode::Human);
        let api_url = server.uri();
        let exit = get(get_options(&api_url, "test_login"), &mut cap.output)
            .await
            .unwrap();

        assert_eq!(exit, ExitCode::Success);
        let out = read(&cap.stdout);
        assert!(out.contains("test_login\n"), "got {out:?}");
        assert!(out.contains("id:         id-1"), "got {out:?}");
        assert!(
            out.contains("reason:     flaky — MRGFY-1234"),
            "got {out:?}"
        );
    }

    #[tokio::test]
    async fn get_by_id_emits_the_record_in_json() {
        let id = "12345678-1234-5678-1234-567812345678";
        let server = MockServer::start().await;
        mount_quarantine_list(&server, one_quarantine_list(id, "test_logout")).await;

        let mut cap = captured(OutputMode::Json);
        let api_url = server.uri();
        // An uppercase id still matches the canonical lowercase record.
        let upper_id = id.to_uppercase();
        let exit = get(get_options(&api_url, &upper_id), &mut cap.output)
            .await
            .unwrap();

        assert_eq!(exit, ExitCode::Success);
        let value: serde_json::Value = serde_json::from_str(&read(&cap.stdout)).unwrap();
        assert_eq!(value["id"], id);
        assert_eq!(value["test_name"], "test_logout");
    }

    #[tokio::test]
    async fn get_unknown_test_is_a_mergify_error() {
        let server = MockServer::start().await;
        mount_quarantine_list(&server, one_quarantine_list("id-1", "test_login")).await;

        let mut cap = captured(OutputMode::Human);
        let api_url = server.uri();
        let err = get(get_options(&api_url, "ghost"), &mut cap.output)
            .await
            .unwrap_err();
        assert!(matches!(err, CliError::MergifyApi(_)), "got {err:?}");
    }
}
