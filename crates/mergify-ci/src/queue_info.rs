//! `mergify ci queue-info` — print the merge-queue batch metadata
//! that's embedded in a merge-queue draft PR.
//!
//! Two ways in:
//!
//! - **No argument (in CI):** read the MQ draft PR from the GitHub
//!   Actions event payload (`$GITHUB_EVENT_PATH`).
//! - **PR URL argument (anywhere):** fetch the PR via the GitHub API
//!   and read the metadata out of its body. The URL host drives the
//!   API base (github.com vs GitHub Enterprise Server).
//!
//! Both paths apply the same MQ-draft gate (`extract_from_event`) and
//! exit with `INVALID_STATE` when the PR isn't an MQ draft.
//!
//! Output is pretty-printed JSON on stdout. When `$GITHUB_OUTPUT` is
//! set (GitHub Actions runner) **and** no URL was given, the command
//! also appends the metadata as `queue_metadata` under a random
//! `ghadelimiter_<uuid>` heredoc, matching the pattern the workflow
//! runtime expects for multi-line outputs. An explicit PR lookup is a
//! local/interactive use and never writes GHA outputs.

use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use mergify_core::ApiFlavor;
use mergify_core::CliError;
use mergify_core::Output;
use mergify_core::auth;
use mergify_core::http::Client;
use mergify_core::pull_request::PullRequestRef;
use url::Url;

use crate::github_event::GitHubEvent;
use crate::github_event::PullRequest;
use crate::queue_metadata::MergeQueueMetadata;
use crate::queue_metadata::detect;
use crate::queue_metadata::extract_from_event;

/// Options for the `ci queue-info` command.
pub struct QueueInfoOptions<'a> {
    /// When set, fetch the PR via the GitHub API instead of reading
    /// the CI event payload.
    pub pull_request: Option<&'a PullRequestRef>,
    /// GitHub token. Resolved through the usual flag → env →
    /// `gh auth token` chain when fetching a PR by URL; ignored on the
    /// in-CI (no-argument) path.
    pub token: Option<&'a str>,
}

/// Run the `ci queue-info` command.
pub async fn run(opts: QueueInfoOptions<'_>, output: &mut dyn Output) -> Result<(), CliError> {
    let metadata = match opts.pull_request {
        Some(pr_ref) => fetch_via_api(pr_ref, opts.token, output).await?,
        None => detect(output)?,
    };
    let Some(metadata) = metadata else {
        return Err(CliError::InvalidState(
            "Not a merge queue draft pull request. \
             queue-info only works on a merge queue draft pull request."
                .to_string(),
        ));
    };

    emit_json(output, &metadata)?;
    // Only the in-CI path emits the GHA output; an explicit PR lookup
    // is a local/interactive use that shouldn't write workflow outputs.
    if opts.pull_request.is_none() {
        write_github_output(&metadata)?;
    }
    Ok(())
}

/// Resolve a token + the right GitHub API base from the PR URL host,
/// fetch the PR, and run it through the same MQ-draft gate the event
/// path uses.
async fn fetch_via_api(
    pr_ref: &PullRequestRef,
    token: Option<&str>,
    output: &mut dyn Output,
) -> Result<Option<MergeQueueMetadata>, CliError> {
    let token = auth::resolve_token(token)?;
    let client = Client::new(github_api_base(&pr_ref.host)?, token, ApiFlavor::GitHub)?;
    fetch_pr_metadata(&client, pr_ref, output).await
}

/// Fetch the PR payload and extract its MQ metadata.
///
/// The GitHub PR JSON deserializes straight into [`PullRequest`]
/// (which ignores unknown fields), so wrapping it in a synthetic
/// [`GitHubEvent`] lets us reuse [`extract_from_event`] verbatim —
/// the title/body gate, the stderr warnings, and the `None`-means-
/// not-an-MQ-draft contract are all shared with the event path.
async fn fetch_pr_metadata(
    client: &Client,
    pr_ref: &PullRequestRef,
    output: &mut dyn Output,
) -> Result<Option<MergeQueueMetadata>, CliError> {
    let path = format!("/repos/{}/pulls/{}", pr_ref.repository, pr_ref.pull_number);
    let pull_request: PullRequest = client.get(&path).await?;
    let event = GitHubEvent {
        pull_request: Some(pull_request),
        ..Default::default()
    };
    Ok(extract_from_event(&event, output)?)
}

/// Map a PR URL host to its GitHub REST API base. `github.com` →
/// `api.github.com`; any other host is treated as GitHub Enterprise
/// Server (`https://<host>/api/v3`). Always https — the inverse of
/// the api→html mapping in `mergify-stack`'s `revision_history`.
fn github_api_base(host: &str) -> Result<Url, CliError> {
    // Hosts are case-insensitive, so a pasted `GitHub.com` must still
    // take the dotcom branch. `host` is already userinfo-free
    // (`parse_pr_url` rejects `@`), so formatting it into the
    // authority is safe.
    let raw = if host.eq_ignore_ascii_case("github.com") {
        "https://api.github.com/".to_string()
    } else {
        format!("https://{host}/api/v3/")
    };
    Url::parse(&raw).map_err(|e| CliError::GitHubApi(format!("invalid GitHub host {host:?}: {e}")))
}

fn emit_json(output: &mut dyn Output, metadata: &MergeQueueMetadata) -> std::io::Result<()> {
    output.emit(metadata, &mut |w: &mut dyn Write| {
        let rendered = serde_json::to_string_pretty(metadata)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        writeln!(w, "{rendered}")
    })
}

fn write_github_output(metadata: &MergeQueueMetadata) -> Result<(), CliError> {
    let Some(path) = env::var("GITHUB_OUTPUT").ok().filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    let delimiter = format!("ghadelimiter_{}", random_delimiter_suffix()?);
    let compact = serde_json::to_string(metadata)
        .map_err(|e| CliError::Generic(format!("failed to serialize queue metadata: {e}")))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(PathBuf::from(path))?;
    writeln!(file, "queue_metadata<<{delimiter}")?;
    writeln!(file, "{compact}")?;
    writeln!(file, "{delimiter}")?;
    Ok(())
}

/// 16 random bytes rendered as 32 lowercase hex chars — enough
/// entropy to be unguessable inside one GitHub Actions step, which
/// is all the heredoc delimiter needs (it just has to be absent
/// from the metadata payload). `getrandom` reads from the OS RNG
/// directly; we don't need the UUID parsing/formatting plumbing
/// that `uuid` adds on top.
fn random_delimiter_suffix() -> Result<String, CliError> {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf)
        .map_err(|e| CliError::Generic(format!("OS random source unavailable: {e}")))?;
    let mut hex = String::with_capacity(buf.len() * 2);
    for b in buf {
        use std::fmt::Write as _;
        write!(hex, "{b:02x}").expect("writing to String is infallible");
    }
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use mergify_core::ExitCode;
    use mergify_test_support::Captured;
    use tempfile::TempDir;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::header;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;

    fn write_event_file(dir: &TempDir, body: &str, title: &str) -> PathBuf {
        let path = dir.path().join("event.json");
        let payload = serde_json::json!({
            "pull_request": {
                "title": title,
                "body": body,
            },
        });
        std::fs::write(&path, serde_json::to_vec(&payload).unwrap()).unwrap();
        path
    }

    fn no_pr() -> QueueInfoOptions<'static> {
        QueueInfoOptions {
            pull_request: None,
            token: None,
        }
    }

    /// Build a client pointed at the mock server — the `run`/
    /// `fetch_via_api` rebuilds the base URL from the PR host
    /// (discarding the mock server's address), so the per-fetch path
    /// is exercised through `fetch_pr_metadata` with an injected
    /// client instead of through `run`.
    fn mock_client(server: &MockServer) -> Client {
        Client::new(
            Url::parse(&server.uri()).unwrap(),
            "test-token".to_string(),
            ApiFlavor::GitHub,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn errors_when_not_in_mq_context() {
        let mut cap = Captured::human();
        let err = temp_env::async_with_vars(
            [
                ("GITHUB_EVENT_NAME", None::<&str>),
                ("GITHUB_EVENT_PATH", None),
            ],
            async { run(no_pr(), &mut cap.output).await.unwrap_err() },
        )
        .await;
        assert!(matches!(err, CliError::InvalidState(_)));
        assert_eq!(err.exit_code(), ExitCode::InvalidState);
    }

    #[tokio::test]
    async fn prints_metadata_for_mq_pr() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_event_file(
            &dir,
            "intro\n```yaml\nchecking_base_sha: abc123\npull_requests:\n  - number: 10\n```",
            "merge queue: batch",
        );

        let mut cap = Captured::human();
        temp_env::async_with_vars(
            [
                ("GITHUB_EVENT_NAME", Some("pull_request")),
                ("GITHUB_EVENT_PATH", Some(path.to_str().unwrap())),
                ("GITHUB_OUTPUT", None),
            ],
            async { run(no_pr(), &mut cap.output).await.unwrap() },
        )
        .await;

        let stdout = cap.stdout();
        assert!(stdout.contains("\"checking_base_sha\": \"abc123\""));
        assert!(stdout.contains("\"number\": 10"));
    }

    #[tokio::test]
    async fn appends_to_github_output_when_set() {
        let dir = tempfile::tempdir().unwrap();
        let event_path = write_event_file(
            &dir,
            "```yaml\nchecking_base_sha: deadbeef\n```",
            "merge queue: tiny",
        );
        let gha_output = dir.path().join("gha_output");

        let mut cap = Captured::human();
        temp_env::async_with_vars(
            [
                ("GITHUB_EVENT_NAME", Some("pull_request")),
                ("GITHUB_EVENT_PATH", Some(event_path.to_str().unwrap())),
                ("GITHUB_OUTPUT", Some(gha_output.to_str().unwrap())),
            ],
            async { run(no_pr(), &mut cap.output).await.unwrap() },
        )
        .await;

        let written = std::fs::read_to_string(&gha_output).unwrap();
        assert!(written.starts_with("queue_metadata<<ghadelimiter_"));
        assert!(written.contains("\"checking_base_sha\":\"deadbeef\""));
    }

    #[test]
    fn github_api_base_maps_dotcom_to_api_host() {
        assert_eq!(
            github_api_base("github.com").unwrap().as_str(),
            "https://api.github.com/",
        );
    }

    #[test]
    fn github_api_base_maps_ghes_host_to_api_v3() {
        assert_eq!(
            github_api_base("ghe.example.com").unwrap().as_str(),
            "https://ghe.example.com/api/v3/",
        );
    }

    #[test]
    fn github_api_base_matches_dotcom_case_insensitively() {
        // Hosts are case-insensitive; a pasted `GitHub.com` must not
        // fall through to the GHES `/api/v3` path.
        assert_eq!(
            github_api_base("GitHub.com").unwrap().as_str(),
            "https://api.github.com/",
        );
    }

    fn pr_ref() -> PullRequestRef {
        PullRequestRef {
            host: "github.com".to_string(),
            repository: "owner/repo".to_string(),
            pull_number: 1234,
        }
    }

    #[tokio::test]
    async fn fetches_pr_and_extracts_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/pulls/1234"))
            // The resolved token must reach GitHub as a bearer header,
            // otherwise private-repo lookups would 404.
            .and(header("Authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "title": "merge queue: batch",
                "body": "intro\n```yaml\nchecking_base_sha: cafef00d\npull_requests:\n  - number: 7\n```",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = mock_client(&server);
        let mut cap = Captured::human();
        let meta = fetch_pr_metadata(&client, &pr_ref(), &mut cap.output)
            .await
            .unwrap()
            .expect("expected MQ metadata");
        assert_eq!(meta.checking_base_sha, "cafef00d");
        assert_eq!(meta.pull_requests[0].number, 7);
    }

    #[tokio::test]
    async fn fetched_non_mq_pr_yields_none() {
        // A regular PR (title not `merge queue: …`) must surface as
        // `None` so `run` maps it to INVALID_STATE — same gate the
        // event path uses.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/pulls/1234"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "title": "feat: something",
                "body": "no metadata here",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = mock_client(&server);
        let mut cap = Captured::human();
        let meta = fetch_pr_metadata(&client, &pr_ref(), &mut cap.output)
            .await
            .unwrap();
        assert!(meta.is_none(), "got: {meta:?}");
    }

    #[tokio::test]
    async fn fetched_pr_api_error_propagates() {
        // A GitHub API failure (e.g. 404 for a wrong PR number) must
        // surface as a GitHubApi error, not a silent None.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/pulls/1234"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Not Found"))
            .mount(&server)
            .await;

        let client = mock_client(&server);
        let mut cap = Captured::human();
        let err = fetch_pr_metadata(&client, &pr_ref(), &mut cap.output)
            .await
            .unwrap_err();
        assert!(matches!(err, CliError::GitHubApi(_)), "got: {err:?}");
    }
}
