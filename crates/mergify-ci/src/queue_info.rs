//! `mergify ci queue-info` — print the merge-queue batch metadata
//! that's embedded in the current merge-queue draft PR.
//!
//! Output is pretty-printed JSON on stdout. When the step isn't
//! running against an MQ draft the command exits with
//! `INVALID_STATE` — same behavior as Python.
//!
//! When `$GITHUB_OUTPUT` is set (GitHub Actions runner), the command
//! also appends the metadata as `queue_metadata` under a random
//! `ghadelimiter_<uuid>` heredoc, matching the pattern the workflow
//! runtime expects for multi-line outputs.

use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use mergify_core::CliError;
use mergify_core::Output;

use crate::queue_metadata::MergeQueueMetadata;
use crate::queue_metadata::detect;

/// Run the `ci queue-info` command.
pub fn run(output: &mut dyn Output) -> Result<(), CliError> {
    let Some(metadata) = detect(output)? else {
        return Err(CliError::InvalidState(
            "Not running in a merge queue context. \
             This command must be run on a merge queue draft pull request."
                .to_string(),
        ));
    };

    emit_json(output, &metadata)?;
    write_github_output(&metadata)?;
    Ok(())
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
    let delimiter = format!("ghadelimiter_{}", uuid::Uuid::new_v4());
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

#[cfg(test)]
mod tests {
    use mergify_core::ExitCode;
    use mergify_core::OutputMode;
    use mergify_core::StdioOutput;
    use tempfile::TempDir;

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

    #[test]
    fn errors_when_not_in_mq_context() {
        let mut cap = make_output();
        let err = temp_env::with_vars_unset(["GITHUB_EVENT_NAME", "GITHUB_EVENT_PATH"], || {
            run(&mut cap.output).unwrap_err()
        });
        assert!(matches!(err, CliError::InvalidState(_)));
        assert_eq!(err.exit_code(), ExitCode::InvalidState);
    }

    #[test]
    fn prints_metadata_for_mq_pr() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_event_file(
            &dir,
            "intro\n```yaml\nchecking_base_sha: abc123\npull_requests:\n  - number: 10\n```",
            "merge queue: batch",
        );

        let mut cap = make_output();
        temp_env::with_vars(
            [
                ("GITHUB_EVENT_NAME", Some("pull_request")),
                ("GITHUB_EVENT_PATH", Some(path.to_str().unwrap())),
                ("GITHUB_OUTPUT", None),
            ],
            || run(&mut cap.output).unwrap(),
        );

        let stdout = String::from_utf8(cap.stdout.lock().unwrap().clone()).unwrap();
        assert!(stdout.contains("\"checking_base_sha\": \"abc123\""));
        assert!(stdout.contains("\"number\": 10"));
    }

    #[test]
    fn appends_to_github_output_when_set() {
        let dir = tempfile::tempdir().unwrap();
        let event_path = write_event_file(
            &dir,
            "```yaml\nchecking_base_sha: deadbeef\n```",
            "merge queue: tiny",
        );
        let gha_output = dir.path().join("gha_output");

        let mut cap = make_output();
        temp_env::with_vars(
            [
                ("GITHUB_EVENT_NAME", Some("pull_request")),
                ("GITHUB_EVENT_PATH", Some(event_path.to_str().unwrap())),
                ("GITHUB_OUTPUT", Some(gha_output.to_str().unwrap())),
            ],
            || run(&mut cap.output).unwrap(),
        );

        let written = std::fs::read_to_string(&gha_output).unwrap();
        assert!(written.starts_with("queue_metadata<<ghadelimiter_"));
        assert!(written.contains("\"checking_base_sha\":\"deadbeef\""));
    }

    struct SharedWriter(SharedBytes);
    impl std::io::Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
