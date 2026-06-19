//! `mergify queue unpause` — resume the merge queue for a
//! repository.
//!
//! Sends a DELETE to ``/v1/repos/<repo>/merge-queue/pause``. When the API
//! responds 404 the command prints "Queue is not currently paused"
//! and exits with `MERGIFY_API_ERROR` — matches Python's behavior.

use std::io::Write;

use mergify_core::CliError;
use mergify_core::CommandContext;
use mergify_core::DeleteOutcome;
use mergify_core::Output;

pub struct UnpauseOptions<'a> {
    pub repository: Option<&'a str>,
    pub token: Option<&'a str>,
    pub api_url: Option<&'a str>,
}

/// Run the `queue unpause` command.
pub async fn run(opts: UnpauseOptions<'_>, output: &mut dyn Output) -> Result<(), CliError> {
    let ctx = CommandContext::resolve(opts.repository, opts.token, opts.api_url)?;

    output.status(&format!(
        "Unpausing merge queue for {repo}…",
        repo = ctx.repository,
    ))?;

    let client = ctx.mergify_client()?;
    let path = format!("/v1/repos/{}/merge-queue/pause", ctx.repository);

    match client.delete_if_exists(&path).await? {
        DeleteOutcome::Deleted => {
            emit_unpaused(output)?;
            Ok(())
        }
        DeleteOutcome::NotFound => Err(CliError::MergifyApi(
            "Queue is not currently paused".to_string(),
        )),
    }
}

fn emit_unpaused(output: &mut dyn Output) -> std::io::Result<()> {
    output.emit(&(), &mut |w: &mut dyn Write| {
        writeln!(w, "Queue unpaused successfully.")
    })
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

    use super::*;

    #[tokio::test]
    async fn run_unpauses_on_2xx() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/repos/owner/repo/merge-queue/pause"))
            .and(header("Authorization", "Bearer t"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = Captured::human();
        let api_url = server.uri();
        run(
            UnpauseOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let stdout = cap.stdout();
        assert!(
            stdout.contains("Queue unpaused successfully."),
            "got: {stdout:?}"
        );
    }

    #[tokio::test]
    async fn run_reports_not_currently_paused_on_404() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/repos/owner/repo/merge-queue/pause"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = Captured::human();
        let api_url = server.uri();
        let err = run(
            UnpauseOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
            },
            &mut cap.output,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CliError::MergifyApi(_)));
        assert!(err.to_string().contains("not currently paused"));
        assert_eq!(err.exit_code(), mergify_core::ExitCode::MergifyApiError);
    }
}
