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
/// Returns a message when `value` exceeds 255 user-visible
/// characters. We count via [`str::chars`] (Unicode scalar values)
/// rather than [`str::len`] (UTF-8 bytes), so non-ASCII reasons
/// such as `"déploiement"` aren't rejected for being below 255
/// chars but above 255 bytes.
pub fn parse_reason(value: &str) -> Result<String, String> {
    if value.chars().count() > MAX_REASON_LEN {
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
    // Both fields are optional defensively: the API has historically
    // tolerated `reason: null` (the deleted Python `QueuePauseResponse`
    // typed it as `str | None`), so the Rust port matches that
    // shape rather than aborting deserialization on a missing or
    // null value.
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    paused_at: Option<String>,
}

/// Run the `queue pause` command.
pub async fn run(opts: PauseOptions<'_>, output: &mut dyn Output) -> Result<(), CliError> {
    // Resolve auth/repo first so the prompt names the *actual* repo
    // (including the `GITHUB_REPOSITORY` fallback) and so a missing
    // repo or token fails loudly *before* we ask for confirmation.
    let repository = auth::resolve_repository(opts.repository)?;
    let token = auth::resolve_token(opts.token)?;
    let api_url = auth::resolve_api_url(opts.api_url)?;

    confirm(opts.yes_i_am_sure, &repository)?;

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

fn confirm(skip: bool, repository: &str) -> Result<(), CliError> {
    if skip {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        return Err(CliError::InvalidState(
            "refusing to pause without confirmation. Pass --yes-i-am-sure to proceed.".to_string(),
        ));
    }
    // Prompt goes to stderr (matches click's `confirm`/`prompt`
    // behavior) so users can pipe stdout cleanly without the
    // prompt text mixed in.
    let mut err = std::io::stderr().lock();
    write!(
        err,
        "You are about to pause the merge queue for {repository}. Proceed? [y/N]: ",
    )
    .map_err(CliError::from)?;
    err.flush().map_err(CliError::from)?;
    drop(err);

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
        match &reason {
            Some(r) => write!(w, "Queue paused: \"{r}\"")?,
            None => write!(w, "Queue paused")?,
        }
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

    #[test]
    fn parse_reason_counts_chars_not_bytes() {
        // 200 user-visible characters but well above 255 *bytes*
        // because each `é` is a 2-byte UTF-8 sequence — we keep it.
        let multibyte = "é".repeat(200);
        assert!(multibyte.len() > MAX_REASON_LEN);
        assert!(multibyte.chars().count() <= MAX_REASON_LEN);
        assert!(parse_reason(&multibyte).is_ok());
    }

    #[test]
    fn confirm_refuses_without_yes_when_non_tty() {
        // Inside `cargo test` stdin is not a TTY — that's the
        // exact branch we want to verify. With `skip = false` the
        // function must short-circuit to `InvalidState` instead of
        // hanging on `read_line`.
        let err = confirm(false, "owner/repo").unwrap_err();
        assert!(
            matches!(err, CliError::InvalidState(_)),
            "expected InvalidState, got {err:?}"
        );
        assert_eq!(err.exit_code(), mergify_core::ExitCode::InvalidState);
        assert!(
            err.to_string().contains("--yes-i-am-sure"),
            "message should mention the override flag, got: {err}"
        );
    }

    #[test]
    fn confirm_skips_when_yes_i_am_sure_is_set() {
        // `skip = true` must bypass even the TTY check — it's the
        // contract of `--yes-i-am-sure`, including in CI where
        // stdin isn't a terminal.
        confirm(true, "owner/repo").unwrap();
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
