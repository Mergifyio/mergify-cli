//! Extract merge-queue batch metadata from a GitHub event payload.
//!
//! The engine embeds the batch info as a ```yaml``` fenced block inside
//! the MQ draft PR body. [`extract_from_event`] reads it from a
//! pull-request event and returns `None` when the event isn't an MQ
//! draft or has no metadata; `git_refs` uses it as one of its base-SHA
//! detection paths. (`queue-info` itself no longer reads the PR body —
//! it reads the engine's git note; see `queue_info`.)
//!
//! The payload is parsed into a generic value rather than a fixed
//! struct, matching how `git::read_note` reads the identical payload
//! off the git note. This is load-bearing, not stylistic: the engine
//! evolves these keys (`draft_pr_number` -> `batch_pr_number`, and it
//! dual-emits both for a deprecation window). A struct with required
//! fields would fail the *whole document* over one renamed key it
//! never even reads, and the caller's `.ok()` would swallow that into
//! `None` — silently costing `git_refs` the `checking_base_sha` it
//! came for and sending CI diffs against the wrong base. Deserializing
//! only what is read keeps the CLI working against any engine version.

use mergify_core::Output;

use crate::github_event::GitHubEvent;

/// Parse the first ```yaml``` fenced block out of `body` into a
/// generic value, keeping every field. Returns `None` when the body
/// has no fenced block, or the block isn't a YAML mapping (a bare
/// scalar or sequence isn't merge-queue metadata).
#[must_use]
pub fn parse_yaml_block(body: &str) -> Option<serde_json::Value> {
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
    // Same parse as the git-note reader — the engine writes this
    // identical payload to both the note and the PR body.
    crate::git::parse_yaml_mapping(&lines.join("\n"))
}

/// Extract MQ metadata from an event payload's pull-request body.
///
/// Emits a warning on `output` (stderr for human mode) when the PR is
/// an MQ draft but the body is missing or lacks the fenced block —
/// matches Python's stderr warnings.
pub fn extract_from_event(
    ev: &GitHubEvent,
    output: &mut dyn Output,
) -> std::io::Result<Option<serde_json::Value>> {
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
        assert_eq!(meta["checking_base_sha"], "abc");
        assert_eq!(meta["pull_requests"][0]["number"], 1);
    }

    #[test]
    fn parse_yaml_block_returns_none_without_block() {
        assert!(parse_yaml_block("just text").is_none());
    }

    #[test]
    fn parse_yaml_block_rejects_non_mapping() {
        // A bare scalar or sequence isn't merge-queue metadata; the
        // caller warns instead of treating it as an empty payload.
        assert!(parse_yaml_block("```yaml\njust a scalar\n```").is_none());
        assert!(parse_yaml_block("```yaml\n- a\n- b\n```").is_none());
    }

    #[test]
    fn parse_yaml_block_survives_every_batch_pr_key_spelling() {
        // The engine renamed `draft_pr_number` -> `batch_pr_number`
        // and dual-emits both for a deprecation window. We never read
        // the key, so all three spellings must yield the same
        // `checking_base_sha` and none may fail the document. Modelling
        // this block with a required field once made a *renamed* key
        // silently cost us the base SHA entirely.
        for batch_keys in [
            "draft_pr_number: 42",
            "batch_pr_number: 42",
            "batch_pr_number: 42\n    draft_pr_number: 42",
        ] {
            let body = format!(
                "prelude\n```yaml\nchecking_base_sha: cafef00d\nprevious_failed_batches:\n  - {batch_keys}\n    checked_pull_requests:\n      - 7\n```"
            );
            let meta = parse_yaml_block(&body)
                .unwrap_or_else(|| panic!("should parse with {batch_keys:?}"));
            assert_eq!(meta["checking_base_sha"], "cafef00d");
        }
    }

    #[test]
    fn parse_yaml_block_survives_unknown_and_missing_keys() {
        // Same invariant, generalised: a key the engine adds later, and
        // an entry missing keys we used to require, both leave
        // `checking_base_sha` reachable.
        let body = "```yaml\nchecking_base_sha: abc\nsomething_new: 1\nprevious_failed_batches:\n  - {}\n```";
        let meta = parse_yaml_block(body).unwrap();
        assert_eq!(meta["checking_base_sha"], "abc");
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
        assert_eq!(meta["checking_base_sha"], "deadbeef");
    }
}
