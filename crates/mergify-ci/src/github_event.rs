//! Deserialization of the GitHub Actions event payload.
//!
//! Mirrors the `pydantic` models in `mergify_cli.ci.github_event`.
//! All structs ignore unknown fields (`serde(default)` + no
//! `deny_unknown_fields` on purpose) so the payload's superset of
//! fields doesn't break us.

use mergify_core::env;
use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GitRef {
    pub sha: String,
    #[serde(default)]
    pub r#ref: Option<String>,
    /// `null` on a pull request's `head` once its fork is deleted.
    #[serde(default)]
    pub repo: Option<RefRepository>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RefRepository {
    /// Optional so that a `repo` without it costs only the trust in
    /// [`PullRequest::is_from_base_repository`], not the whole event.
    #[serde(default)]
    pub id: Option<u64>,
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

impl PullRequest {
    /// Whether the head branch lives in the base repository, which is
    /// where the engine always opens its merge queue drafts.
    ///
    /// This is what lets the CLI trust merge queue metadata, never the
    /// `merge queue: ` title, which the author picks (MRGFY-8854). It
    /// rules out a fork, whose author writes the code, the body, and,
    /// when the workflow checks the fork out as `origin`, the git note
    /// too. It does not rule out a reader opening a pull request from a
    /// branch someone else pushed: they write its title and body, but
    /// not its code. The branch prefix and the author are no better:
    /// `queue_branch_prefix` and `draft_bot_account` are configurable.
    ///
    /// `false` when either side has no repository id, including the
    /// `null` head of a deleted fork.
    #[must_use]
    pub fn is_from_base_repository(&self) -> bool {
        let repo_id = |r: &Option<GitRef>| r.as_ref()?.repo.as_ref()?.id;
        matches!(
            (repo_id(&self.head), repo_id(&self.base)),
            (Some(head), Some(base)) if head == base
        )
    }
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
    let event_name = env::var_non_empty("GITHUB_EVENT_NAME")?;
    let event_path = env::var_non_empty("GITHUB_EVENT_PATH")?;
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
    fn is_from_base_repository_needs_both_ids_and_equal() {
        let pr = |raw: &str| serde_json::from_str::<PullRequest>(raw).unwrap();
        assert!(pr(r#"{"head": {"sha": "h", "repo": {"id": 1}}, "base": {"sha": "b", "repo": {"id": 1}}}"#).is_from_base_repository());
        for raw in [
            r#"{"head": {"sha": "h", "repo": {"id": 2}}, "base": {"sha": "b", "repo": {"id": 1}}}"#,
            r#"{"head": {"sha": "h", "repo": null}, "base": {"sha": "b", "repo": {"id": 1}}}"#,
            r#"{"head": {"sha": "h", "repo": {"id": 1}}, "base": {"sha": "b"}}"#,
            r#"{"head": {"sha": "h"}, "base": {"sha": "b"}}"#,
            r#"{"head": {"sha": "h", "repo": {}}, "base": {"sha": "b", "repo": {}}}"#,
        ] {
            assert!(!pr(raw).is_from_base_repository(), "admitted {raw}");
        }
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
