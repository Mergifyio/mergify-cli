//! `mergify ci scopes-send` — report the scopes detected for a pull
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
//! ``--all`` marks the pull request as impacting every scope: the
//! request body carries ``all_scopes: true`` alongside the concrete
//! ``scopes`` list, and the merge queue treats the pull request as
//! a barrier. The flag is also honored when the ``--scopes-json``
//! file carries ``"all_scopes": true``.
//!
//! Pull-request number and repository are explicit flags that fall
//! back to environment (``GITHUB_REPOSITORY``, ``GITHUB_EVENT_PATH``
//! with ``.pull_request.number``). When neither source yields a
//! pull-request number the command prints a skip message and
//! returns success — matches Python's "no PR, nothing to send"
//! behavior.
//!
//! The report is addressed by the pull request's **head SHA**
//! (``--head-sha``, else the CI environment) so it says which
//! revision it was computed for: scopes computed for one head do not
//! describe another, and the number-addressed record cannot tell the
//! difference — whichever upload lands last wins (MRGFY-8884). The
//! number-addressed endpoint stays the fallback for the two cases
//! where the SHA route is not available: no head SHA could be
//! resolved, or the Mergify deployment is older than the endpoint and
//! answers 404.
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
    pub all_scopes: bool,
    pub head_sha: Option<&'a str>,
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
    let mut all_scopes = opts.all_scopes;
    if let Some(path) = scopes_json_path {
        let dump = load_scopes_json(path)?;
        scopes.extend(dump.scopes);
        all_scopes = all_scopes || dump.all_scopes;
    }
    if let Some(path) = opts.scopes_file {
        scopes.extend(read_scopes_text_file(path)?);
    }

    let head_sha = resolve_head_sha(opts.head_sha);
    if head_sha.is_none() {
        output.status(
            "No pull request head SHA detected, reporting against the pull \
             request number instead: the scopes will not say which revision \
             they were computed for.",
        )?;
    }

    let all_scopes_note = if all_scopes {
        " (impacting all scopes)"
    } else {
        ""
    };
    let target = match &head_sha {
        Some(sha) => format!("commit {sha}"),
        None => format!("pull request #{pull_request}"),
    };
    output.status(&format!(
        "Sending {} scope(s){all_scopes_note} for {target} to {api_url}…",
        scopes.len(),
    ))?;

    let client = HttpClient::new(api_url, token, ApiFlavor::Mergify)?;
    let body = SendScopesRequest {
        scopes: &scopes,
        all_scopes,
    };

    // Both calls below discard the response: each endpoint answers an
    // empty body on success, which a `::<Value>` variant would surface
    // as "parse response JSON: error decoding response body".
    if let Some(sha) = &head_sha {
        let path = format!("/v1/repos/{repository}/commits/{sha}/scopes");
        let reported_against_the_commit = client.put_no_response_if_exists(&path, &body).await?;
        if reported_against_the_commit {
            return Ok(());
        }
        // A Mergify older than the endpoint answers 404, and so does
        // one that cannot see the repository — the message reports what
        // was observed rather than diagnosing which, and the fallback
        // surfaces the second case as its own error when the
        // pull-request route 404s too.
        output.status(
            "The commit scopes endpoint answered 404, falling back to the \
             pull request one: the scopes will not say which revision they \
             were computed for.",
        )?;
    }

    // The fallback stays on POST rather than the pull-request route's
    // own PUT: a deployment old enough to 404 the commit endpoint may
    // be old enough to 404 that PUT too (it landed four months
    // earlier), which would defeat the point of falling back.
    let path = format!("/v1/repos/{repository}/pulls/{pull_request}/scopes");
    client.post_no_response(&path, &body).await?;

    Ok(())
}

/// The pull request head SHA to report the scopes against, or `None`
/// when nothing names one.
///
/// Both sources are re-checked here rather than trusted. Detection
/// filters its own result and clap's `parse_head_sha` rejects a bad
/// `--head-sha`, but `ScopesSendOptions` is public: the value lands in
/// a request path, and `Client::join` resolves `..` segments, so this
/// is the same rule `detector::resolve_repository` applies to every
/// source of its own path segment.
///
/// Lowercased for the same reason `tests_quarantine` normalizes an id
/// before pathing it: the engine folds case, but a row stored under a
/// spelling no reader looks for is worse than a rejected one, and only
/// one of the two spellings should ever be sent.
fn resolve_head_sha(explicit: Option<&str>) -> Option<String> {
    explicit
        .map(ToString::to_string)
        .or_else(detector::get_github_pull_request_head_sha)
        .filter(|sha| detector::is_sha1_object_name(sha))
        .map(|sha| sha.to_lowercase())
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
    // Optional so today's `ci scopes --write` output (which doesn't
    // emit it yet) keeps parsing; the config-driven detection that
    // will produce it is tracked separately (MRGFY-7892).
    #[serde(default)]
    all_scopes: bool,
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
    all_scopes: bool,
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
    use crate::testing::write_github_event;

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
        // A dump without the field means "not an all-scopes PR" —
        // today's `ci scopes --write` output doesn't emit it.
        assert!(!got.all_scopes);
    }

    #[test]
    fn load_scopes_json_parses_all_scopes_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("scopes.json");
        fs::write(&path, r#"{"scopes": ["backend"], "all_scopes": true}"#).unwrap();
        let got = load_scopes_json(&path).unwrap();
        assert!(got.all_scopes);
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
                    all_scopes: false,
                    head_sha: None,
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
            .and(body_json(
                serde_json::json!({"scopes": ["a"], "all_scopes": false}),
            ))
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
                        all_scopes: false,
                        head_sha: None,
                    },
                    &mut cap.output,
                )
                .await
                .unwrap();
            },
        )
        .await;

        // No `BUILDKITE_COMMIT`, so nothing names the head: the report
        // falls back to the pull request number, and says so.
        let err = cap.stderr();
        assert!(
            err.contains("No pull request head SHA detected"),
            "got: {err:?}"
        );
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
                "all_scopes": false,
            })))
            .respond_with(ResponseTemplate::new(200))
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
                all_scopes: false,
                head_sha: None,
            },
            &mut cap.output,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn run_sends_all_scopes_true_when_flag_set() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/repos/owner/repo/pulls/5/scopes"))
            .and(body_json(serde_json::json!({
                "scopes": ["backend"],
                "all_scopes": true,
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = Captured::human();
        let api_url = server.uri();
        let direct = vec!["backend".to_string()];

        run(
            ScopesSendOptions {
                repository: Some("owner/repo"),
                pull_request: Some(5),
                token: Some("t"),
                api_url: Some(&api_url),
                scopes: &direct,
                scopes_json: None,
                scopes_file: None,
                deprecated_file: None,
                all_scopes: true,
                head_sha: None,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let err = cap.stderr();
        assert!(err.contains("impacting all scopes"), "got: {err:?}");
    }

    #[tokio::test]
    async fn run_honors_all_scopes_from_scopes_json_file() {
        // Forward-compat: when `ci scopes --write` starts emitting
        // `all_scopes` (config-driven detection, MRGFY-7892), the
        // flag must flow through without requiring `--all`.
        let server = MockServer::start().await;
        let tmp = tempfile::tempdir().unwrap();
        let json_path = tmp.path().join("scopes.json");
        fs::write(&json_path, r#"{"scopes": ["a"], "all_scopes": true}"#).unwrap();

        Mock::given(method("POST"))
            .and(path("/v1/repos/owner/repo/pulls/6/scopes"))
            .and(body_json(serde_json::json!({
                "scopes": ["a"],
                "all_scopes": true,
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = Captured::human();
        let api_url = server.uri();

        run(
            ScopesSendOptions {
                repository: Some("owner/repo"),
                pull_request: Some(6),
                token: Some("t"),
                api_url: Some(&api_url),
                scopes: &[],
                scopes_json: Some(&json_path),
                scopes_file: None,
                deprecated_file: None,
                all_scopes: false,
                head_sha: None,
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
                all_scopes: false,
                head_sha: None,
            },
            &mut cap.output,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn run_reports_against_the_head_sha_from_the_github_event() {
        let head = "feedface00000000000000000000000000000000";
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path(format!("/v1/repos/owner/repo/commits/{head}/scopes")))
            .and(body_json(
                serde_json::json!({"scopes": ["a"], "all_scopes": false}),
            ))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().unwrap();
        let event_path = write_github_event(
            tmp.path(),
            &serde_json::json!({"pull_request": {"number": 42, "head": {"sha": head}}}),
        );
        let mut cap = Captured::human();
        let api_url = server.uri();
        let direct = vec!["a".to_string()];

        with_ci_env_async(
            &[
                ("GITHUB_ACTIONS", Some("true")),
                ("GITHUB_EVENT_NAME", Some("pull_request")),
                ("GITHUB_EVENT_PATH", Some(event_path.to_str().unwrap())),
                ("GITHUB_REPOSITORY", Some("owner/repo")),
                // The merge commit, not the head — see
                // `detector::get_github_pull_request_head_sha`.
                (
                    "GITHUB_SHA",
                    Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"),
                ),
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
                        all_scopes: false,
                        head_sha: None,
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
    async fn run_prefers_the_explicit_head_sha_over_detection() {
        let detected = "feedface00000000000000000000000000000000";
        // Uppercase on the way in: the path segment must still be the one
        // spelling a reader looks for.
        let explicit = "0123456789ABCDEF0123456789ABCDEF01234567";
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path(format!(
                "/v1/repos/owner/repo/commits/{}/scopes",
                explicit.to_lowercase(),
            )))
            .and(body_json(serde_json::json!({
                "scopes": ["backend"],
                "all_scopes": true,
            })))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().unwrap();
        let event_path = write_github_event(
            tmp.path(),
            &serde_json::json!({"pull_request": {"number": 42, "head": {"sha": detected}}}),
        );
        let mut cap = Captured::human();
        let api_url = server.uri();
        let direct = vec!["backend".to_string()];

        with_ci_env_async(
            &[
                ("GITHUB_ACTIONS", Some("true")),
                ("GITHUB_EVENT_NAME", Some("pull_request")),
                ("GITHUB_EVENT_PATH", Some(event_path.to_str().unwrap())),
                ("GITHUB_REPOSITORY", Some("owner/repo")),
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
                        all_scopes: true,
                        head_sha: Some(explicit),
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
    async fn run_falls_back_to_the_pull_request_when_the_commit_endpoint_is_unknown() {
        // A Mergify older than the commit endpoint 404s the route. The
        // upload has to keep working there, at the precision that
        // deployment already had.
        let head = "feedface00000000000000000000000000000000";
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path(format!("/v1/repos/owner/repo/commits/{head}/scopes")))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/repos/owner/repo/pulls/42/scopes"))
            .and(body_json(
                serde_json::json!({"scopes": ["a"], "all_scopes": false}),
            ))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = Captured::human();
        let api_url = server.uri();
        let direct = vec!["a".to_string()];

        run(
            ScopesSendOptions {
                repository: Some("owner/repo"),
                pull_request: Some(42),
                token: Some("t"),
                api_url: Some(&api_url),
                scopes: &direct,
                scopes_json: None,
                scopes_file: None,
                deprecated_file: None,
                all_scopes: false,
                head_sha: Some(head),
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let err = cap.stderr();
        assert!(
            err.contains("commit scopes endpoint answered 404"),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn run_warns_on_deprecated_file_flag() {
        let server = MockServer::start().await;
        let tmp = tempfile::tempdir().unwrap();
        let json_path = tmp.path().join("legacy.json");
        fs::write(&json_path, r#"{"scopes": ["x"]}"#).unwrap();

        Mock::given(method("POST"))
            .and(path("/v1/repos/owner/repo/pulls/1/scopes"))
            .respond_with(ResponseTemplate::new(200))
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
                all_scopes: false,
                head_sha: None,
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
            .and(body_json(
                serde_json::json!({"scopes": ["a"], "all_scopes": false}),
            ))
            .respond_with(ResponseTemplate::new(200))
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
                all_scopes: false,
                head_sha: None,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let err = cap.stderr();
        assert!(err.contains("--file is deprecated"), "got: {err:?}");
    }
}
