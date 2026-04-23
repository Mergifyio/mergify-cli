//! Extract merge-queue batch metadata from a GitHub event payload.
//!
//! Mirrors `mergify_cli.ci.queue.metadata`. The engine publishes the
//! batch info as a ```yaml``` fenced block inside the MQ draft PR
//! body. `detect` returns `None` when the current event has no such
//! metadata — callers either fall back to other detection paths
//! (`git_refs`) or surface it as an `INVALID_STATE` (`queue_info`).

use mergify_core::Output;
use serde::Deserialize;
use serde::Serialize;

use crate::github_event::GitHubEvent;
use crate::github_event::PULL_REQUEST_EVENTS;
use crate::github_event::load as load_event;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeQueuePullRequest {
    pub number: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeQueueBatchFailed {
    pub draft_pr_number: u64,
    pub checked_pull_requests: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeQueueMetadata {
    pub checking_base_sha: String,
    #[serde(default)]
    pub pull_requests: Vec<MergeQueuePullRequest>,
    #[serde(default)]
    pub previous_failed_batches: Vec<MergeQueueBatchFailed>,
}

/// Parse the first ```yaml``` fenced block out of `body` and try to
/// read a `MergeQueueMetadata` out of it. Returns `None` when the
/// body has no fenced block or the YAML payload is the wrong shape.
#[must_use]
pub fn parse_yaml_block(body: &str) -> Option<MergeQueueMetadata> {
    let mut inside = false;
    let mut lines: Vec<&str> = Vec::new();
    for line in body.lines() {
        if !inside {
            if line.starts_with("```yaml") {
                inside = true;
            }
        } else if line.starts_with("```") {
            break;
        } else {
            lines.push(line);
        }
    }
    if lines.is_empty() {
        return None;
    }
    serde_yaml_ng::from_str(&lines.join("\n")).ok()
}

/// Extract MQ metadata from an event payload's pull-request body.
///
/// Emits a warning on `output` (stderr for human mode) when the PR is
/// an MQ draft but the body is missing or lacks the fenced block —
/// matches Python's stderr warnings.
pub fn extract_from_event(
    ev: &GitHubEvent,
    output: &mut dyn Output,
) -> std::io::Result<Option<MergeQueueMetadata>> {
    let Some(pr) = &ev.pull_request else {
        return Ok(None);
    };
    let Some(title) = pr.title.as_deref() else {
        return Ok(None);
    };
    if !title.starts_with("merge queue: ") {
        return Ok(None);
    }
    let Some(body) = pr.body.as_deref() else {
        output.status("WARNING: MQ pull request without body, skipping metadata extraction")?;
        return Ok(None);
    };
    let parsed = parse_yaml_block(body);
    if parsed.is_none() {
        output.status(
            "WARNING: MQ pull request body without Mergify metadata, skipping metadata extraction",
        )?;
    }
    Ok(parsed)
}

/// Load the current event and extract merge-queue metadata.
///
/// Returns `None` when not in a pull-request event or when no MQ
/// metadata is attached to the event's PR. Callers decide how to
/// treat that `None` (skip, error, fall back).
pub fn detect(output: &mut dyn Output) -> std::io::Result<Option<MergeQueueMetadata>> {
    let Some((event_name, event)) = load_event() else {
        return Ok(None);
    };
    if !PULL_REQUEST_EVENTS.contains(&event_name.as_str()) {
        return Ok(None);
    }
    extract_from_event(&event, output)
}

#[cfg(test)]
mod tests {
    use mergify_core::OutputMode;
    use mergify_core::StdioOutput;

    use super::*;

    type SharedBytes = std::sync::Arc<std::sync::Mutex<Vec<u8>>>;

    struct Captured {
        output: StdioOutput,
        stderr: SharedBytes,
    }

    fn make_output() -> Captured {
        let stdout: SharedBytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let stderr: SharedBytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let output = StdioOutput::with_sinks(
            OutputMode::Human,
            SharedWriter(std::sync::Arc::clone(&stdout)),
            SharedWriter(std::sync::Arc::clone(&stderr)),
        );
        Captured { output, stderr }
    }

    #[test]
    fn parse_yaml_block_extracts_metadata() {
        let body = "prelude\n\n```yaml\nchecking_base_sha: abc\npull_requests:\n  - number: 1\n```\ntrailing";
        let meta = parse_yaml_block(body).unwrap();
        assert_eq!(meta.checking_base_sha, "abc");
        assert_eq!(meta.pull_requests.len(), 1);
        assert_eq!(meta.pull_requests[0].number, 1);
    }

    #[test]
    fn parse_yaml_block_returns_none_without_block() {
        assert!(parse_yaml_block("just text").is_none());
    }

    #[test]
    fn extract_ignores_non_mq_pr() {
        let ev = GitHubEvent {
            pull_request: Some(crate::github_event::PullRequest {
                title: Some("feat: something".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut cap = make_output();
        let result = extract_from_event(&ev, &mut cap.output).unwrap();
        assert!(result.is_none());
        assert!(cap.stderr.lock().unwrap().is_empty());
    }

    #[test]
    fn extract_warns_on_mq_pr_without_body() {
        let ev = GitHubEvent {
            pull_request: Some(crate::github_event::PullRequest {
                title: Some("merge queue: deploy".into()),
                body: None,
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut cap = make_output();
        let result = extract_from_event(&ev, &mut cap.output).unwrap();
        assert!(result.is_none());
        let stderr = String::from_utf8(cap.stderr.lock().unwrap().clone()).unwrap();
        assert!(stderr.contains("without body"), "got: {stderr:?}");
    }

    #[test]
    fn extract_returns_metadata_for_mq_pr() {
        let body = "blah\n```yaml\nchecking_base_sha: deadbeef\n```";
        let ev = GitHubEvent {
            pull_request: Some(crate::github_event::PullRequest {
                title: Some("merge queue: batch".into()),
                body: Some(body.into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut cap = make_output();
        let meta = extract_from_event(&ev, &mut cap.output).unwrap().unwrap();
        assert_eq!(meta.checking_base_sha, "deadbeef");
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
