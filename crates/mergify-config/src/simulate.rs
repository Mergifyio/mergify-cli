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
use mergify_core::pull_request::PullRequestRef;
use serde::Deserialize;
use serde::Serialize;

use crate::paths::resolve_config_path;

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
            host: "github.com".into(),
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
