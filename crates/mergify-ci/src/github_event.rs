//! Deserialization of the GitHub Actions event payload.
//!
//! Mirrors the `pydantic` models in `mergify_cli.ci.github_event`.
//! All structs ignore unknown fields (`serde(default)` + no
//! `deny_unknown_fields` on purpose) so the payload's superset of
//! fields doesn't break us.

use std::env;
use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GitRef {
    pub sha: String,
    #[serde(default)]
    pub r#ref: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PullRequest {
    #[serde(default)]
    pub number: Option<u64>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub base: Option<GitRef>,
    #[serde(default)]
    pub head: Option<GitRef>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Repository {
    #[serde(default)]
    pub default_branch: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GitHubEvent {
    #[serde(default)]
    pub pull_request: Option<PullRequest>,
    #[serde(default)]
    pub repository: Option<Repository>,
    #[serde(default)]
    pub before: Option<String>,
    #[serde(default)]
    pub after: Option<String>,
}

/// Events that carry a pull request in their payload.
pub const PULL_REQUEST_EVENTS: &[&str] = &[
    "pull_request",
    "pull_request_review",
    "pull_request_review_comment",
    "pull_request_target",
];

/// Load the event payload from `GITHUB_EVENT_PATH`, keyed by
/// `GITHUB_EVENT_NAME`.
///
/// Returns `None` when either env var is missing, the file does not
/// exist, or the JSON cannot be parsed — mirrors Python's
/// `GitHubEventNotFoundError` being converted to a fallback.
#[must_use]
pub fn load() -> Option<(String, GitHubEvent)> {
    let event_name = env::var("GITHUB_EVENT_NAME")
        .ok()
        .filter(|s| !s.is_empty())?;
    let event_path = env::var("GITHUB_EVENT_PATH")
        .ok()
        .filter(|s| !s.is_empty())?;
    let path = PathBuf::from(event_path);
    if !path.is_file() {
        return None;
    }
    let raw = std::fs::read_to_string(&path).ok()?;
    let event: GitHubEvent = serde_json::from_str(&raw).ok()?;
    Some((event_name, event))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_minimal_event() {
        let raw = r#"{"pull_request": {"number": 42}}"#;
        let ev: GitHubEvent = serde_json::from_str(raw).unwrap();
        assert_eq!(ev.pull_request.unwrap().number, Some(42));
    }

    #[test]
    fn deserialize_ignores_unknown_fields() {
        let raw = r#"{"pull_request": {"number": 7, "unknown": "x"}, "foo": 1}"#;
        let ev: GitHubEvent = serde_json::from_str(raw).unwrap();
        assert_eq!(ev.pull_request.unwrap().number, Some(7));
    }

    #[test]
    fn deserialize_push_event_shape() {
        let raw = r#"{"before": "a", "after": "b", "repository": {"default_branch": "main"}}"#;
        let ev: GitHubEvent = serde_json::from_str(raw).unwrap();
        assert_eq!(ev.before.as_deref(), Some("a"));
        assert_eq!(ev.after.as_deref(), Some("b"));
        assert_eq!(
            ev.repository.unwrap().default_branch.as_deref(),
            Some("main")
        );
    }
}
