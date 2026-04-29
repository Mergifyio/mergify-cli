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
//! order as the Python CLI: explicit flag, then env var, then
//! default. Missing token falls back from ``MERGIFY_TOKEN`` to
//! ``GITHUB_TOKEN``. The ``gh auth token`` subprocess fallback from
//! Python isn't ported yet — if neither env var is set the command
//! errors out.

use std::env;
use std::io::Write;
use std::path::Path;

use mergify_core::ApiFlavor;
use mergify_core::CliError;
use mergify_core::HttpClient;
use mergify_core::Output;
use serde::Deserialize;
use serde::Serialize;
use url::Url;

use crate::paths::resolve_config_path;

const DEFAULT_API_URL: &str = "https://api.mergify.com";

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

/// Resolve the Mergify API bearer token.
///
/// Precedence: explicit `--token`, then `MERGIFY_TOKEN`, then
/// `GITHUB_TOKEN`. Errors out when none of those are set.
fn resolve_token(explicit: Option<&str>) -> Result<String, CliError> {
    if let Some(token) = explicit.filter(|t| !t.is_empty()) {
        return Ok(token.to_string());
    }
    for env_name in ["MERGIFY_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(value) = env::var(env_name) {
            if !value.is_empty() {
                return Ok(value);
            }
        }
    }
    Err(CliError::Configuration(
        "please set the 'MERGIFY_TOKEN' or 'GITHUB_TOKEN' environment variable, \
         or pass --token explicitly"
            .to_string(),
    ))
}

/// Resolve the Mergify API base URL. Falls back to `MERGIFY_API_URL`
/// env var, then to the default `https://api.mergify.com`.
fn resolve_api_url(explicit: Option<&str>) -> Result<Url, CliError> {
    let raw = explicit
        .map(str::to_string)
        .or_else(|| env::var("MERGIFY_API_URL").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_API_URL.to_string());
    Url::parse(&raw).map_err(|e| CliError::Configuration(format!("invalid --api-url {raw:?}: {e}")))
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

    let token = resolve_token(opts.token)?;
    let api_url = resolve_api_url(opts.api_url)?;

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
    let title = response.title.clone();
    let summary = response.summary.clone();
    output.emit(&(), &mut |w: &mut dyn Write| {
        writeln!(w, "{title}")?;
        writeln!(w)?;
        // Intentional drift from Python: we print raw Markdown
        // instead of rich-rendering it. Machine-readable output is
        // still locked; human rendering is flexible per the compat
        // contract.
        writeln!(w, "{summary}")
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use mergify_core::OutputMode;
    use mergify_core::StdioOutput;
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

    #[test]
    fn resolve_token_prefers_explicit_over_env() {
        temp_env::with_vars(
            [
                ("MERGIFY_TOKEN", Some("from-env")),
                ("GITHUB_TOKEN", Some("github-env")),
            ],
            || {
                assert_eq!(resolve_token(Some("from-cli")).unwrap(), "from-cli");
            },
        );
    }

    #[test]
    fn resolve_token_falls_back_to_mergify_env() {
        temp_env::with_vars(
            [
                ("MERGIFY_TOKEN", Some("mergify-env")),
                ("GITHUB_TOKEN", Some("github-env")),
            ],
            || {
                assert_eq!(resolve_token(None).unwrap(), "mergify-env");
            },
        );
    }

    #[test]
    fn resolve_token_falls_back_to_github_env() {
        temp_env::with_vars(
            [
                ("MERGIFY_TOKEN", None::<&str>),
                ("GITHUB_TOKEN", Some("github-env")),
            ],
            || {
                assert_eq!(resolve_token(None).unwrap(), "github-env");
            },
        );
    }

    #[test]
    fn resolve_token_errors_when_no_source_available() {
        temp_env::with_vars(
            [
                ("MERGIFY_TOKEN", None::<&str>),
                ("GITHUB_TOKEN", None::<&str>),
            ],
            || {
                let err = resolve_token(None).unwrap_err();
                assert!(matches!(err, CliError::Configuration(_)));
                assert!(err.to_string().contains("MERGIFY_TOKEN"));
            },
        );
    }

    #[test]
    fn resolve_api_url_uses_default_when_unset() {
        temp_env::with_vars([("MERGIFY_API_URL", None::<&str>)], || {
            assert_eq!(
                resolve_api_url(None).unwrap().as_str(),
                "https://api.mergify.com/",
            );
        });
    }

    #[test]
    fn resolve_api_url_prefers_explicit() {
        temp_env::with_vars([("MERGIFY_API_URL", Some("https://from.env"))], || {
            assert_eq!(
                resolve_api_url(Some("https://from.cli")).unwrap().as_str(),
                "https://from.cli/",
            );
        });
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

        let stdout: std::sync::Arc<std::sync::Mutex<Vec<u8>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let stderr: std::sync::Arc<std::sync::Mutex<Vec<u8>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut output = StdioOutput::with_sinks(
            OutputMode::Human,
            SharedWriter(std::sync::Arc::clone(&stdout)),
            SharedWriter(std::sync::Arc::clone(&stderr)),
        );

        run(
            SimulateOptions {
                pull_request: &pull_request,
                config_file: Some(&config_path),
                token: Some("test-token"),
                api_url: Some(&api_url),
            },
            &mut output,
        )
        .await
        .unwrap();

        let stdout_bytes = stdout.lock().unwrap().clone();
        let stdout_str = String::from_utf8(stdout_bytes).unwrap();
        assert!(
            stdout_str.contains("Would merge immediately"),
            "expected title in output: {stdout_str:?}",
        );
        assert!(
            stdout_str.contains("All conditions pass."),
            "expected summary in output: {stdout_str:?}",
        );
    }

    struct SharedWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
