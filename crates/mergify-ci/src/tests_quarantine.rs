//! `mergify tests quarantine` / `mergify tests unquarantine` — add a
//! test to, or remove it from, the CI Insights quarantine through the
//! public quarantine API.
//!
//! `quarantine` addresses a test by its fully qualified name.
//! `unquarantine` accepts either the quarantine id (deleted directly,
//! as the DELETE endpoint is keyed by it) or a test name. The
//! quarantine resource has the composite primary key
//! `(repository, test_name)`, so a name resolves to at most one
//! quarantine per repository — `unquarantine` relies on this when it
//! looks the name up in the list endpoint to recover the id.

use std::io::{self, Write};

use mergify_core::ApiFlavor;
use mergify_core::CliError;
use mergify_core::DeleteOutcome;
use mergify_core::ExitCode;
use mergify_core::HttpClient;
use mergify_core::Output;
use mergify_core::auth;
use serde::Deserialize;
use serde::Serialize;

use crate::detector::split_owner_repo;

pub struct QuarantineOptions<'a> {
    pub repository: &'a str,
    pub test_name: &'a str,
    pub reason: &'a str,
    pub branch: Option<&'a str>,
    pub token: Option<&'a str>,
    pub api_url: Option<&'a str>,
}

pub struct UnquarantineOptions<'a> {
    pub repository: &'a str,
    /// A test name or a quarantine id. A UUID-shaped value is taken as
    /// the quarantine id; anything else is treated as a test name.
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
    let (owner, repo) = split_owner_repo(opts.repository)?;
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
    let (owner, repo) = split_owner_repo(opts.repository)?;
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
    let list: QuarantineList = client.get(&list_path).await?;
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

#[derive(Deserialize)]
struct QuarantineList {
    quarantined_tests: Vec<QuarantineListItem>,
}

#[derive(Deserialize)]
struct QuarantineListItem {
    id: String,
    test_name: String,
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
            repository: "owner/repo",
            test_name,
            reason,
            branch,
            token: Some("test-token"),
            api_url: Some(api_url),
        }
    }

    fn unquarantine_options<'a>(api_url: &'a str, name_or_id: &'a str) -> UnquarantineOptions<'a> {
        UnquarantineOptions {
            repository: "owner/repo",
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
            repository: "not-a-slash-pair",
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
            repository: "not-a-slash-pair",
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
}
