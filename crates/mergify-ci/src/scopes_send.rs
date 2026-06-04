//! `mergify ci scopes-send` — POST the scopes detected for a pull
//! request to Mergify.
//!
//! Scopes can come from three sources (combined):
//!
//! - one or more ``--scope <name>`` flags
//! - ``--scopes-json <file>``: JSON with a ``{"scopes": [...]}``
//!   shape (the output of ``mergify ci scopes --write``)
//! - ``--scopes-file <file>``: plain text, one scope per line
//!
//! ``--file`` is the deprecated alias for ``--scopes-json`` and
//! emits a warning to stderr; it is hidden from the public help.
//!
//! Pull-request number and repository are explicit flags that fall
//! back to environment (``GITHUB_REPOSITORY``, ``GITHUB_EVENT_PATH``
//! with ``.pull_request.number``). When neither source yields a
//! pull-request number the command prints a skip message and
//! returns success — matches Python's "no PR, nothing to send"
//! behavior.
//!
//! Auth + API URL resolution goes through `mergify_core::auth`,
//! which adds a `gh auth token` fallback (matches Python's
//! `utils.get_default_token`) and a `git config remote.origin.url`
//! fallback for the repository slug (matches
//! `utils.get_default_repository`).

use std::path::Path;

use mergify_core::ApiFlavor;
use mergify_core::CliError;
use mergify_core::HttpClient;
use mergify_core::Output;
use mergify_core::auth;
use serde::Deserialize;
use serde::Serialize;

use crate::detector;

pub struct ScopesSendOptions<'a> {
    pub repository: Option<&'a str>,
    pub pull_request: Option<u64>,
    pub token: Option<&'a str>,
    pub api_url: Option<&'a str>,
    pub scopes: &'a [String],
    pub scopes_json: Option<&'a Path>,
    pub scopes_file: Option<&'a Path>,
    pub deprecated_file: Option<&'a Path>,
}

/// Run the `ci scopes-send` command.
pub async fn run(opts: ScopesSendOptions<'_>, output: &mut dyn Output) -> Result<(), CliError> {
    let Some(pull_request) = resolve_pull_request(opts.pull_request)? else {
        output.status("No pull request number detected, skipping scopes upload.")?;
        return Ok(());
    };

    let repository = detector::resolve_repository(opts.repository)?;
    let token = auth::resolve_token(opts.token)?;
    let api_url = auth::resolve_api_url(opts.api_url)?;

    // Whenever the deprecated `--file` flag is supplied, surface
    // the deprecation warning — even when `--scopes-json` is also
    // set and ends up taking precedence. Users need to know `--file`
    // will be going away regardless of whether the current
    // invocation actually relies on it.
    if opts.deprecated_file.is_some() {
        output.status("Warning: --file is deprecated, use --scopes-json instead.")?;
    }
    let scopes_json_path = opts.scopes_json.or(opts.deprecated_file);

    let mut scopes: Vec<String> = opts.scopes.to_vec();
    if let Some(path) = scopes_json_path {
        let dump = load_scopes_json(path)?;
        scopes.extend(dump.scopes);
    }
    if let Some(path) = opts.scopes_file {
        scopes.extend(read_scopes_text_file(path)?);
    }

    output.status(&format!("Sending {} scope(s) to {api_url}…", scopes.len()))?;

    let client = HttpClient::new(api_url, token, ApiFlavor::Mergify)?;
    let path = format!("/v1/repos/{repository}/pulls/{pull_request}/scopes");
    // The endpoint returns an empty body on success — `post::<Value>`
    // would surface that as "parse response JSON: error decoding
    // response body". We only need to know the request was 2xx.
    client
        .post_no_response(&path, &SendScopesRequest { scopes: &scopes })
        .await?;

    Ok(())
}

fn resolve_pull_request(explicit: Option<u64>) -> Result<Option<u64>, CliError> {
    if let Some(n) = explicit {
        return Ok(Some(n));
    }
    detector::get_github_pull_request_number()
}

#[derive(Deserialize)]
struct DetectedScopesFile {
    scopes: Vec<String>,
}

fn load_scopes_json(path: &Path) -> Result<DetectedScopesFile, CliError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| CliError::Configuration(format!("cannot read {}: {e}", path.display())))?;
    serde_json::from_str(&text).map_err(|e| {
        CliError::Configuration(format!(
            "cannot parse scopes JSON from {}: {e}",
            path.display(),
        ))
    })
}

fn read_scopes_text_file(path: &Path) -> Result<Vec<String>, CliError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| CliError::Configuration(format!("cannot read {}: {e}", path.display())))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect())
}

#[derive(Serialize)]
struct SendScopesRequest<'a> {
    scopes: &'a [String],
}

#[cfg(test)]
mod tests {
    use std::fs;

    use mergify_test_support::Captured;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::body_json;
    use wiremock::matchers::header;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;
    use crate::testing::with_ci_env;
    use crate::testing::with_ci_env_async;

    #[test]
    fn resolve_pull_request_prefers_explicit() {
        with_ci_env(&[], || {
            assert_eq!(resolve_pull_request(Some(7)).unwrap(), Some(7));
        });
    }

    // Provider-aware detection (Buildkite/CircleCI/Jenkins/GHA) has
    // unit coverage in `detector::tests`. This module keeps only the
    // wrapper-level checks: explicit-flag precedence and error
    // wrapping.

    #[test]
    fn load_scopes_json_parses_dump_format() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("scopes.json");
        fs::write(&path, r#"{"scopes": ["backend", "frontend"]}"#).unwrap();
        let got = load_scopes_json(&path).unwrap();
        assert_eq!(got.scopes, vec!["backend", "frontend"]);
    }

    #[test]
    fn read_scopes_text_file_strips_blanks_and_trims() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("scopes.txt");
        fs::write(&path, "  backend \n\n frontend\n  \n").unwrap();
        let got = read_scopes_text_file(&path).unwrap();
        assert_eq!(got, vec!["backend", "frontend"]);
    }

    #[tokio::test]
    async fn run_skips_when_no_pull_request_detected() {
        let mut cap = Captured::human();
        with_ci_env_async(&[("GITHUB_REPOSITORY", Some("owner/repo"))], async {
            run(
                ScopesSendOptions {
                    repository: None,
                    pull_request: None,
                    token: Some("test-token"),
                    api_url: Some("https://api.mergify.com"),
                    scopes: &[],
                    scopes_json: None,
                    scopes_file: None,
                    deprecated_file: None,
                },
                &mut cap.output,
            )
            .await
            .unwrap();
        })
        .await;
        let stderr_str = cap.stderr();
        assert!(
            stderr_str.contains("skipping"),
            "expected skip message, got {stderr_str:?}"
        );
    }

    #[tokio::test]
    async fn run_resolves_buildkite_repo_and_pull_request_from_env() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/repos/owner/repo/pulls/99/scopes"))
            .and(body_json(serde_json::json!({"scopes": ["a"]})))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = Captured::human();
        let api_url = server.uri();
        let direct = vec!["a".to_string()];

        with_ci_env_async(
            &[
                ("BUILDKITE", Some("true")),
                ("BUILDKITE_REPO", Some("git@github.com:owner/repo.git")),
                ("BUILDKITE_PULL_REQUEST", Some("99")),
            ],
            async {
                run(
                    ScopesSendOptions {
                        repository: None,
                        pull_request: None,
                        token: Some("t"),
                        api_url: Some(&api_url),
                        scopes: &direct,
                        scopes_json: None,
                        scopes_file: None,
                        deprecated_file: None,
                    },
                    &mut cap.output,
                )
                .await
                .unwrap();
            },
        )
        .await;
    }

    #[tokio::test]
    async fn run_posts_combined_scopes_from_all_sources() {
        let server = MockServer::start().await;
        let tmp = tempfile::tempdir().unwrap();
        let json_path = tmp.path().join("scopes.json");
        fs::write(&json_path, r#"{"scopes": ["fromjson"]}"#).unwrap();
        let txt_path = tmp.path().join("scopes.txt");
        fs::write(&txt_path, "fromtext\n").unwrap();

        Mock::given(method("POST"))
            .and(path("/v1/repos/owner/repo/pulls/42/scopes"))
            .and(header("Authorization", "Bearer test-token"))
            .and(body_json(serde_json::json!({
                "scopes": ["direct", "fromjson", "fromtext"],
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = Captured::human();
        let api_url = server.uri();
        let direct = vec!["direct".to_string()];

        run(
            ScopesSendOptions {
                repository: Some("owner/repo"),
                pull_request: Some(42),
                token: Some("test-token"),
                api_url: Some(&api_url),
                scopes: &direct,
                scopes_json: Some(&json_path),
                scopes_file: Some(&txt_path),
                deprecated_file: None,
            },
            &mut cap.output,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn run_succeeds_when_server_returns_empty_body() {
        // Regression: the Mergify scopes-send endpoint returns an
        // empty body on success. Earlier the Rust port tried to
        // deserialize it as `serde_json::Value` and surfaced
        // "parse response JSON: error decoding response body".
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/repos/owner/repo/pulls/7/scopes"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = Captured::human();
        let api_url = server.uri();

        run(
            ScopesSendOptions {
                repository: Some("owner/repo"),
                pull_request: Some(7),
                token: Some("t"),
                api_url: Some(&api_url),
                scopes: &[],
                scopes_json: None,
                scopes_file: None,
                deprecated_file: None,
            },
            &mut cap.output,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn run_warns_on_deprecated_file_flag() {
        let server = MockServer::start().await;
        let tmp = tempfile::tempdir().unwrap();
        let json_path = tmp.path().join("legacy.json");
        fs::write(&json_path, r#"{"scopes": ["x"]}"#).unwrap();

        Mock::given(method("POST"))
            .and(path("/v1/repos/owner/repo/pulls/1/scopes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        let mut cap = Captured::human();
        let api_url = server.uri();

        run(
            ScopesSendOptions {
                repository: Some("owner/repo"),
                pull_request: Some(1),
                token: Some("t"),
                api_url: Some(&api_url),
                scopes: &[],
                scopes_json: None,
                scopes_file: None,
                deprecated_file: Some(&json_path),
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let err = cap.stderr();
        assert!(err.contains("--file is deprecated"), "got: {err:?}");
    }

    #[tokio::test]
    async fn run_warns_when_both_scopes_json_and_deprecated_file_provided() {
        // The deprecation warning must surface even when
        // `--scopes-json` is also set (and ends up taking
        // precedence) — users shouldn't have to remove the modern
        // flag to discover that `--file` is on its way out.
        let server = MockServer::start().await;
        let tmp = tempfile::tempdir().unwrap();
        let json_path = tmp.path().join("modern.json");
        fs::write(&json_path, r#"{"scopes": ["a"]}"#).unwrap();
        let deprecated_path = tmp.path().join("legacy.json");
        fs::write(&deprecated_path, r#"{"scopes": ["b"]}"#).unwrap();

        Mock::given(method("POST"))
            .and(path("/v1/repos/owner/repo/pulls/1/scopes"))
            .and(body_json(serde_json::json!({"scopes": ["a"]})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = Captured::human();
        let api_url = server.uri();

        run(
            ScopesSendOptions {
                repository: Some("owner/repo"),
                pull_request: Some(1),
                token: Some("t"),
                api_url: Some(&api_url),
                scopes: &[],
                scopes_json: Some(&json_path),
                scopes_file: None,
                deprecated_file: Some(&deprecated_path),
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let err = cap.stderr();
        assert!(err.contains("--file is deprecated"), "got: {err:?}");
    }
}
