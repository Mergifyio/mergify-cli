//! Extract merge-queue batch metadata from a GitHub event payload.
//!
//! The engine embeds the batch info as a ```yaml``` fenced block inside
//! the MQ draft PR body. [`extract_from_event`] reads it from a
//! pull-request event and returns `None` when the event isn't an MQ
//! draft or has no metadata; `git_refs` uses it as one of its base-SHA
//! detection paths. (`queue-info` itself no longer reads the PR body —
//! it reads the engine's git note; see `queue_info`.)

use mergify_core::Output;
use serde::Deserialize;
use serde::Serialize;

use crate::github_event::GitHubEvent;

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

#[cfg(test)]
mod tests {
    use mergify_test_support::Captured;

    use super::*;

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
        let mut cap = Captured::human();
        let result = extract_from_event(&ev, &mut cap.output).unwrap();
        assert!(result.is_none());
        assert!(cap.stderr().is_empty());
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
        let mut cap = Captured::human();
        let result = extract_from_event(&ev, &mut cap.output).unwrap();
        assert!(result.is_none());
        let stderr = cap.stderr();
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
        let mut cap = Captured::human();
        let meta = extract_from_event(&ev, &mut cap.output).unwrap().unwrap();
        assert_eq!(meta.checking_base_sha, "deadbeef");
    }
}
