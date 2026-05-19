//! `mergify config simulate` — simulate Mergify actions on a pull
//! request using the local configuration.
//!
//! The command:
//! 1. Parses the pull-request URL into ``(owner/repo, number)``.
//! 2. Resolves the config file (same paths as `config validate`).
//! 3. Reads the YAML as a raw string (no parsing — the Mergify
//!    simulator accepts the text verbatim).
//! 4. POSTs to
//!    ``<api-url>/v1/repos/<repo>/pulls/<number>/simulator`` with
//!    ``{"mergify_yml": <content>}``.
//! 5. Prints the simulator's title + summary.
//!
//! Token / api-url / config-file all follow the same resolution
//! order as the Python CLI (`mergify_core::auth`): explicit flag,
//! then env var, then `gh auth token` for the bearer, then the
//! default API URL.

use std::io::Write;
use std::path::Path;

use mergify_core::ApiFlavor;
use mergify_core::CliError;
use mergify_core::HttpClient;
use mergify_core::Output;
use mergify_core::auth;
use serde::Deserialize;
use serde::Serialize;

use crate::paths::resolve_config_path;

/// Deserialized shape of the `(owner/repo, number)` pair parsed from
/// a pull-request URL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PullRequestRef {
    pub repository: String,
    pub pull_number: u64,
}

/// Clap value-parser for the positional PR URL argument.
///
/// Returning `Err(String)` makes clap exit with status 2 (argument
/// validation error) rather than our CLI's `ConfigurationError` —
/// matching the Python CLI's behavior where `_parse_pr_url` raises
/// `click.BadParameter` (also exit 2).
///
/// # Errors
///
/// Returns a human-readable message when `url` is not a valid
/// GitHub-style pull request URL.
pub fn parse_pr_url(url: &str) -> Result<PullRequestRef, String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| format!("Invalid pull request URL: {url}"))?;
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() != 5 || parts[3] != "pull" {
        return Err(format!("Invalid pull request URL: {url}"));
    }
    let [host, owner, repo, _pull, number] = [parts[0], parts[1], parts[2], parts[3], parts[4]];
    if host.is_empty() || owner.is_empty() || repo.is_empty() {
        return Err(format!("Invalid pull request URL: {url}"));
    }
    let pull_number: u64 = number
        .parse()
        .map_err(|_| format!("Invalid pull request URL: {url}"))?;
    Ok(PullRequestRef {
        repository: format!("{owner}/{repo}"),
        pull_number,
    })
}

#[derive(Serialize)]
struct SimulatorRequest<'a> {
    mergify_yml: &'a str,
}

#[derive(Deserialize)]
struct SimulatorResponse {
    title: String,
    summary: String,
}

pub struct SimulateOptions<'a> {
    pub pull_request: &'a PullRequestRef,
    pub config_file: Option<&'a Path>,
    pub token: Option<&'a str>,
    pub api_url: Option<&'a str>,
}

/// Run the `config simulate` command.
pub async fn run(opts: SimulateOptions<'_>, output: &mut dyn Output) -> Result<(), CliError> {
    let config_path = resolve_config_path(opts.config_file)?;
    let mergify_yml = std::fs::read_to_string(&config_path).map_err(|e| {
        CliError::Configuration(format!("cannot read {}: {e}", config_path.display()))
    })?;

    let token = auth::resolve_token(opts.token)?;
    let api_url = auth::resolve_api_url(opts.api_url)?;

    output.status(&format!("Simulating against {api_url}…"))?;

    let client = HttpClient::new(api_url, token, ApiFlavor::Mergify)?;
    let path = format!(
        "/v1/repos/{}/pulls/{}/simulator",
        opts.pull_request.repository, opts.pull_request.pull_number,
    );
    let response: SimulatorResponse = client
        .post(
            &path,
            &SimulatorRequest {
                mergify_yml: &mergify_yml,
            },
        )
        .await?;

    emit_result(output, &response)?;
    Ok(())
}

fn emit_result(output: &mut dyn Output, response: &SimulatorResponse) -> std::io::Result<()> {
    output.emit(&(), &mut |w: &mut dyn Write| {
        writeln!(w, "{title}", title = response.title)?;
        writeln!(w)?;
        // Intentional drift from Python: we print raw Markdown
        // instead of rich-rendering it. Machine-readable output is
        // still locked; human rendering is flexible per the compat
        // contract.
        writeln!(w, "{summary}", summary = response.summary)
    })
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

    #[test]
    fn parse_pr_url_accepts_canonical_github_url() {
        let got = parse_pr_url("https://github.com/owner/repo/pull/42").unwrap();
        assert_eq!(got.repository, "owner/repo");
        assert_eq!(got.pull_number, 42);
    }

    #[test]
    fn parse_pr_url_rejects_non_pull_path() {
        assert!(parse_pr_url("https://github.com/owner/repo/issues/42").is_err());
    }

    #[test]
    fn parse_pr_url_rejects_trailing_segments() {
        assert!(parse_pr_url("https://github.com/owner/repo/pull/42/files").is_err());
    }

    #[test]
    fn parse_pr_url_rejects_non_numeric_pull_number() {
        assert!(parse_pr_url("https://github.com/owner/repo/pull/abc").is_err());
    }

    #[test]
    fn parse_pr_url_rejects_missing_scheme() {
        assert!(parse_pr_url("github.com/owner/repo/pull/42").is_err());
    }

    #[test]
    fn parse_pr_url_rejects_empty_owner() {
        assert!(parse_pr_url("https://github.com//repo/pull/42").is_err());
    }

    #[tokio::test]
    async fn run_posts_config_and_prints_simulator_result() {
        let server = MockServer::start().await;
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join(".mergify.yml");
        fs::write(&config_path, "pull_request_rules: []\n").unwrap();

        Mock::given(method("POST"))
            .and(path("/v1/repos/owner/repo/pulls/42/simulator"))
            .and(header("Authorization", "Bearer test-token"))
            .and(body_json(serde_json::json!({
                "mergify_yml": "pull_request_rules: []\n",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "title": "Would merge immediately",
                "summary": "All conditions pass.",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let pull_request = PullRequestRef {
            repository: "owner/repo".into(),
            pull_number: 42,
        };
        let api_url = server.uri();

        let mut cap = Captured::human();
        run(
            SimulateOptions {
                pull_request: &pull_request,
                config_file: Some(&config_path),
                token: Some("test-token"),
                api_url: Some(&api_url),
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let stdout_str = cap.stdout();
        assert!(
            stdout_str.contains("Would merge immediately"),
            "expected title in output: {stdout_str:?}",
        );
        assert!(
            stdout_str.contains("All conditions pass."),
            "expected summary in output: {stdout_str:?}",
        );
    }
}
