//! `mergify queue pause` — pause the merge queue for a repository.
//!
//! PUTs ``{"reason": "..."}`` to
//! ``/v1/repos/<repo>/merge-queue/pause``. Prints a "Queue paused"
//! confirmation with the reason (and the raw pause timestamp if
//! the API returned one).
//!
//! Confirmation flow:
//!
//! - ``--yes-i-am-sure`` skips the prompt.
//! - Interactive (TTY): asks "Proceed? [y/N]"; anything other than
//!   "y"/"yes" aborts with a generic error.
//! - Non-interactive (no TTY, no ``--yes-i-am-sure``): refuses with
//!   an ``INVALID_STATE`` error matching Python's behavior.

use std::io::IsTerminal;
use std::io::Write;

use mergify_core::ApiFlavor;
use mergify_core::CliError;
use mergify_core::HttpClient;
use mergify_core::Output;
use serde::Deserialize;
use serde::Serialize;

use crate::auth;

const MAX_REASON_LEN: usize = 255;

pub struct PauseOptions<'a> {
    pub repository: Option<&'a str>,
    pub token: Option<&'a str>,
    pub api_url: Option<&'a str>,
    pub reason: &'a str,
    pub yes_i_am_sure: bool,
}

/// Clap value-parser for the positional `--reason` flag.
///
/// # Errors
///
/// Returns a message when `value` exceeds 255 characters.
pub fn parse_reason(value: &str) -> Result<String, String> {
    if value.len() > MAX_REASON_LEN {
        Err("must be 255 characters or fewer".to_string())
    } else {
        Ok(value.to_string())
    }
}

#[derive(Serialize)]
struct PauseRequest<'a> {
    reason: &'a str,
}

#[derive(Deserialize)]
struct PauseResponse {
    reason: String,
    #[serde(default)]
    paused_at: Option<String>,
}

/// Run the `queue pause` command.
pub async fn run(opts: PauseOptions<'_>, output: &mut dyn Output) -> Result<(), CliError> {
    confirm(opts.yes_i_am_sure, opts.repository)?;

    let repository = auth::resolve_repository(opts.repository)?;
    let token = auth::resolve_token(opts.token)?;
    let api_url = auth::resolve_api_url(opts.api_url)?;

    output.status(&format!("Pausing merge queue for {repository}…"))?;

    let client = HttpClient::new(api_url, token, ApiFlavor::Mergify)?;
    let path = format!("/v1/repos/{repository}/merge-queue/pause");
    let resp: PauseResponse = client
        .put(
            &path,
            &PauseRequest {
                reason: opts.reason,
            },
        )
        .await?;

    emit_confirmation(output, &resp)?;
    Ok(())
}

fn confirm(skip: bool, repository: Option<&str>) -> Result<(), CliError> {
    if skip {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        return Err(CliError::InvalidState(
            "refusing to pause without confirmation. Pass --yes-i-am-sure to proceed.".to_string(),
        ));
    }
    let repo_hint = repository.unwrap_or("this repository");
    let mut out = std::io::stdout().lock();
    write!(
        out,
        "You are about to pause the merge queue for {repo_hint}. Proceed? [y/N]: ",
    )
    .map_err(CliError::from)?;
    out.flush().map_err(CliError::from)?;
    drop(out);

    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(CliError::from)?;
    match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(()),
        _ => Err(CliError::Generic("aborted by user".to_string())),
    }
}

fn emit_confirmation(output: &mut dyn Output, response: &PauseResponse) -> std::io::Result<()> {
    let reason = response.reason.clone();
    let paused_at = response.paused_at.clone();
    output.emit(&(), &mut |w: &mut dyn Write| {
        write!(w, "Queue paused: \"{reason}\"")?;
        if let Some(ts) = &paused_at {
            write!(w, " (since {ts})")?;
        }
        writeln!(w)
    })
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn parse_reason_accepts_short() {
        assert_eq!(
            parse_reason("deploying hotfix").unwrap(),
            "deploying hotfix"
        );
    }

    #[test]
    fn parse_reason_rejects_over_255() {
        let long = "a".repeat(256);
        assert!(parse_reason(&long).is_err());
    }

    #[tokio::test]
    async fn run_pauses_and_prints_confirmation() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/v1/repos/owner/repo/merge-queue/pause"))
            .and(header("Authorization", "Bearer t"))
            .and(body_json(serde_json::json!({"reason": "deploy freeze"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "reason": "deploy freeze",
                "paused_at": "2026-04-23T12:34:56Z",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = make_output();
        let api_url = server.uri();
        run(
            PauseOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                reason: "deploy freeze",
                yes_i_am_sure: true,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let stdout = String::from_utf8(cap.stdout.lock().unwrap().clone()).unwrap();
        assert!(stdout.contains("Queue paused"), "got: {stdout:?}");
        assert!(stdout.contains("deploy freeze"), "got: {stdout:?}");
        assert!(stdout.contains("2026-04-23"), "got: {stdout:?}");
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
