//! `mergify queue unpause` — resume the merge queue for a
//! repository.
//!
//! DELETEs ``/v1/repos/<repo>/merge-queue/pause``. When the API
//! responds 404 the command prints "Queue is not currently paused"
//! and exits with `MERGIFY_API_ERROR` — matches Python's behavior.

use std::io::Write;

use mergify_core::ApiFlavor;
use mergify_core::CliError;
use mergify_core::DeleteOutcome;
use mergify_core::HttpClient;
use mergify_core::Output;

use crate::auth;

pub struct UnpauseOptions<'a> {
    pub repository: Option<&'a str>,
    pub token: Option<&'a str>,
    pub api_url: Option<&'a str>,
}

/// Run the `queue unpause` command.
pub async fn run(opts: UnpauseOptions<'_>, output: &mut dyn Output) -> Result<(), CliError> {
    let repository = auth::resolve_repository(opts.repository)?;
    let token = auth::resolve_token(opts.token)?;
    let api_url = auth::resolve_api_url(opts.api_url)?;

    output.status(&format!("Unpausing merge queue for {repository}…"))?;

    let client = HttpClient::new(api_url, token, ApiFlavor::Mergify)?;
    let path = format!("/v1/repos/{repository}/merge-queue/pause");

    match client.delete_if_exists(&path).await? {
        DeleteOutcome::Deleted => {
            emit_resumed(output)?;
            Ok(())
        }
        DeleteOutcome::NotFound => Err(CliError::MergifyApi(
            "Queue is not currently paused".to_string(),
        )),
    }
}

fn emit_resumed(output: &mut dyn Output) -> std::io::Result<()> {
    output.emit(&(), &mut |w: &mut dyn Write| writeln!(w, "Queue resumed."))
}

#[cfg(test)]
mod tests {
    use mergify_core::OutputMode;
    use mergify_core::StdioOutput;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::header;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;

    type SharedBytes = std::sync::Arc<std::sync::Mutex<Vec<u8>>>;

    struct Captured {
        output: StdioOutput,
        stdout: SharedBytes,
    }

    fn make_output() -> Captured {
        let stdout: SharedBytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let stderr: SharedBytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let output = StdioOutput::with_sinks(
            OutputMode::Human,
            SharedWriter(std::sync::Arc::clone(&stdout)),
            SharedWriter(std::sync::Arc::clone(&stderr)),
        );
        Captured { output, stdout }
    }

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

        let mut cap = make_output();
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

        let stdout = String::from_utf8(cap.stdout.lock().unwrap().clone()).unwrap();
        assert!(stdout.contains("Queue resumed"), "got: {stdout:?}");
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

        let mut cap = make_output();
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

    struct SharedWriter(SharedBytes);
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
