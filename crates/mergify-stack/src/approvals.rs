//! Decide whether `stack push` should rebase the local stack onto
//! trunk, or skip the rebase to avoid dismissing already-given
//! approvals.
//!
//! The trade-off: rebasing a stack force-pushes every branch in
//! it, which dismisses every "Approved" review on every PR in the
//! stack. That's painful when reviewers have already signed off
//! — but skipping the rebase is dangerous when the bottom of the
//! stack has a real merge conflict with trunk. This module
//! encodes the same decision Python makes:
//!
//! - `--skip-rebase` short-circuits to *don't rebase*.
//! - `--force-rebase` short-circuits to *rebase*.
//! - Otherwise: check approvals on every live PR; if any are
//!   approved and the bottom PR doesn't have a real conflict,
//!   skip the rebase; otherwise rebase.
//!
//! Ported from `mergify_cli/stack/approvals.py`.

use std::time::Duration;

use mergify_core::{CliError, HttpClient};
use serde::Deserialize;
use serde_json::Value;

use crate::changes::{Action, Changes};

const MERGEABLE_RETRY_DELAY: Duration = Duration::from_secs(1);
const CONFLICT_STATE: &str = "dirty";

/// Why [`decide_rebase`] reached its conclusion. Surfaced to the
/// CLI so the user can read the reason in the `stack push`
/// output ("Skipping rebase: PRs are approved").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebaseReason {
    /// `--skip-rebase` was passed.
    ExplicitSkip,
    /// `--force-rebase` was passed.
    Forced,
    /// Some PRs are approved, but the bottom PR has a real merge
    /// conflict with trunk — rebase wins.
    ConflictOverride,
    /// PRs are approved and there's no conflict → skip the
    /// rebase to preserve the approvals.
    SkippedForApprovals,
    /// No approvals to preserve → rebase as usual.
    NoApprovals,
}

/// Result of [`decide_rebase`]: the boolean for the push code to
/// act on, the reason for logging, and the list of approved PR
/// payloads (carried as-is from the GitHub fetch so the caller can
/// surface PR numbers / titles in the rebase-skipped notice).
#[derive(Debug, Clone)]
pub struct RebaseDecision {
    pub should_rebase: bool,
    pub reason: RebaseReason,
    pub approved_pulls: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct Review {
    state: String,
    user: Option<ReviewUser>,
}

#[derive(Debug, Deserialize)]
struct ReviewUser {
    login: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PullState {
    mergeable_state: Option<String>,
    mergeable: Option<bool>,
}

/// Is this PR currently approved?
///
/// "Currently" means: take the latest non-comment, non-pending
/// review per reviewer (so a reviewer who once approved and then
/// requested changes counts as "requested changes"), and report
/// whether any of those latest reviews is APPROVED.
pub async fn pull_is_approved(
    client: &HttpClient,
    user: &str,
    repo: &str,
    pull_number: u64,
) -> Result<bool, CliError> {
    let path = format!("/repos/{user}/{repo}/pulls/{pull_number}/reviews");
    let reviews: Vec<Review> = client.get(&path).await?;

    // Walk reviews in API order — GitHub returns them oldest-first
    // — so the last write wins per reviewer.
    let mut latest_by_reviewer: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for review in reviews {
        if review.state == "COMMENTED" || review.state == "PENDING" {
            continue;
        }
        if let Some(login) = review.user.and_then(|u| u.login)
            && !login.is_empty()
        {
            latest_by_reviewer.insert(login, review.state);
        }
    }
    Ok(latest_by_reviewer.values().any(|s| s == "APPROVED"))
}

/// Fetch the set of approved PR numbers from `pulls`. Sequential
/// across PRs to stay under GitHub's secondary rate-limit on the
/// PR-reviews endpoint (same trade-off as [`crate::remote_changes`]).
pub async fn fetch_approved_pull_numbers(
    client: &HttpClient,
    user: &str,
    repo: &str,
    pulls: &[Value],
) -> Result<std::collections::HashSet<u64>, CliError> {
    let mut approved = std::collections::HashSet::new();
    for pull in pulls {
        let Some(number) = pull.get("number").and_then(Value::as_u64) else {
            continue;
        };
        if pull_is_approved(client, user, repo, number).await? {
            approved.insert(number);
        }
    }
    Ok(approved)
}

/// Does the bottom PR have a real merge conflict with trunk?
///
/// GitHub computes `mergeable` lazily — the first GET on a PR
/// often returns `null`. The Python implementation waits 1s and
/// retries once; we do the same. Any HTTP error coerces to
/// `false` (matches Python's `except httpx.HTTPError: return
/// False`) so a transient API hiccup can't force a surprise
/// rebase.
pub async fn bottom_pull_has_conflict(
    client: &HttpClient,
    user: &str,
    repo: &str,
    bottom_pull: Option<&Value>,
) -> bool {
    let Some(pull) = bottom_pull else {
        return false;
    };
    let Some(number) = pull.get("number").and_then(Value::as_u64) else {
        return false;
    };
    let path = format!("/repos/{user}/{repo}/pulls/{number}");

    let Ok(data) = client.get::<PullState>(&path).await else {
        return false;
    };
    let data = if data.mergeable.is_none() {
        tokio::time::sleep(MERGEABLE_RETRY_DELAY).await;
        let Ok(retried) = client.get::<PullState>(&path).await else {
            return false;
        };
        retried
    } else {
        data
    };

    data.mergeable_state.as_deref() == Some(CONFLICT_STATE)
}

/// Make the rebase / no-rebase decision for one `stack push`.
///
/// The full precedence: explicit flags first (so `--skip-rebase` /
/// `--force-rebase` are no-questions-asked), then the
/// approvals-vs-conflict trade-off described in the module docs.
pub async fn decide_rebase(
    client: &HttpClient,
    user: &str,
    repo: &str,
    planned_changes: &Changes,
    skip_rebase: bool,
    force_rebase: bool,
) -> Result<RebaseDecision, CliError> {
    if skip_rebase {
        return Ok(RebaseDecision {
            should_rebase: false,
            reason: RebaseReason::ExplicitSkip,
            approved_pulls: Vec::new(),
        });
    }
    if force_rebase {
        return Ok(RebaseDecision {
            should_rebase: true,
            reason: RebaseReason::Forced,
            approved_pulls: Vec::new(),
        });
    }

    // All live PRs (skip-merged ones are already on trunk and
    // their approvals don't matter for the push that's about
    // to happen).
    let stack_pulls: Vec<Value> = planned_changes
        .locals
        .iter()
        .filter(|c| c.action != Action::SkipMerged)
        .filter_map(|c| c.pull.clone())
        .collect();

    let approved_numbers = fetch_approved_pull_numbers(client, user, repo, &stack_pulls).await?;
    let approved_pulls: Vec<Value> = stack_pulls
        .iter()
        .filter(|p| {
            p.get("number")
                .and_then(Value::as_u64)
                .is_some_and(|n| approved_numbers.contains(&n))
        })
        .cloned()
        .collect();

    // Bottom PR = first live (non-merged) change in the stack.
    // `create` actions don't have a `pull` yet, so `bottom_pull`
    // stays None and `bottom_pull_has_conflict` short-circuits
    // to false — there's nothing on the server to be in conflict
    // with anyway.
    let bottom_pull = planned_changes
        .locals
        .iter()
        .find(|c| c.action != Action::SkipMerged)
        .and_then(|c| c.pull.as_ref());
    let has_conflict = bottom_pull_has_conflict(client, user, repo, bottom_pull).await;

    if !approved_pulls.is_empty() && has_conflict {
        return Ok(RebaseDecision {
            should_rebase: true,
            reason: RebaseReason::ConflictOverride,
            approved_pulls,
        });
    }
    if !approved_pulls.is_empty() {
        return Ok(RebaseDecision {
            should_rebase: false,
            reason: RebaseReason::SkippedForApprovals,
            approved_pulls,
        });
    }
    Ok(RebaseDecision {
        should_rebase: true,
        reason: RebaseReason::NoApprovals,
        approved_pulls: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mergify_core::{ApiFlavor, HttpClient};
    use serde_json::json;
    use url::Url;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_client(server: &MockServer) -> HttpClient {
        HttpClient::new(
            Url::parse(&server.uri()).expect("valid url"),
            "token",
            ApiFlavor::GitHub,
        )
        .expect("client")
    }

    #[tokio::test]
    async fn pull_is_approved_uses_latest_review_per_reviewer() {
        // Same reviewer first APPROVED then CHANGES_REQUESTED —
        // the latest wins. Without "latest per reviewer" semantics
        // a stale APPROVE would falsely keep the PR in the
        // approved set and the rebase skip would dismiss real
        // change requests.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls/1/reviews"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"state": "APPROVED", "user": {"login": "alice"}},
                {"state": "COMMENTED", "user": {"login": "bob"}},
                {"state": "CHANGES_REQUESTED", "user": {"login": "alice"}},
            ])))
            .mount(&server)
            .await;

        let client = make_client(&server);
        assert!(!pull_is_approved(&client, "o", "r", 1).await.unwrap());
    }

    #[tokio::test]
    async fn pull_is_approved_returns_true_when_any_reviewer_has_latest_approval() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls/1/reviews"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"state": "CHANGES_REQUESTED", "user": {"login": "alice"}},
                {"state": "APPROVED", "user": {"login": "bob"}},
            ])))
            .mount(&server)
            .await;

        let client = make_client(&server);
        assert!(pull_is_approved(&client, "o", "r", 1).await.unwrap());
    }

    #[tokio::test]
    async fn bottom_pull_has_conflict_returns_true_on_dirty_state() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "mergeable": false,
                "mergeable_state": "dirty",
            })))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let pull = json!({"number": 1});
        assert!(bottom_pull_has_conflict(&client, "o", "r", Some(&pull)).await);
    }

    #[tokio::test]
    async fn bottom_pull_has_conflict_returns_false_for_clean_state() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "mergeable": true,
                "mergeable_state": "clean",
            })))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let pull = json!({"number": 1});
        assert!(!bottom_pull_has_conflict(&client, "o", "r", Some(&pull)).await);
    }

    #[tokio::test]
    async fn bottom_pull_has_conflict_short_circuits_when_no_bottom_pull() {
        // The bottom-of-stack is a fresh `create` so there's no
        // PR to query — no network call, no conflict.
        let server = MockServer::start().await;
        let client = make_client(&server);
        assert!(!bottom_pull_has_conflict(&client, "o", "r", None).await);
    }

    #[tokio::test]
    async fn bottom_pull_has_conflict_returns_false_on_http_error() {
        // GitHub flake → caller must not be coerced into rebasing
        // when an approval would have skipped it. Python catches
        // the error and returns False; we mirror that.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls/1"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let pull = json!({"number": 1});
        assert!(!bottom_pull_has_conflict(&client, "o", "r", Some(&pull)).await);
    }

    fn local_change_with_pull(action: Action, pull: Option<Value>) -> crate::changes::LocalChange {
        crate::changes::LocalChange {
            commit_sha: "deadbeef".to_string(),
            title: "t".to_string(),
            message: "m".to_string(),
            change_id: "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            action,
            pull,
            note: String::new(),
        }
    }

    #[tokio::test]
    async fn decide_rebase_explicit_skip_short_circuits() {
        let server = MockServer::start().await;
        let client = make_client(&server);
        let planned = Changes {
            locals: vec![local_change_with_pull(
                Action::Update,
                Some(json!({"number": 1})),
            )],
            orphans: Vec::new(),
        };
        let d = decide_rebase(&client, "o", "r", &planned, true, false)
            .await
            .unwrap();
        assert!(!d.should_rebase);
        assert_eq!(d.reason, RebaseReason::ExplicitSkip);
        assert!(d.approved_pulls.is_empty());
    }

    #[tokio::test]
    async fn decide_rebase_force_short_circuits_without_querying_github() {
        // No mocks registered — if we queried we'd 404, so this
        // also proves the short-circuit short-circuits.
        let server = MockServer::start().await;
        let client = make_client(&server);
        let planned = Changes {
            locals: vec![local_change_with_pull(
                Action::Update,
                Some(json!({"number": 1})),
            )],
            orphans: Vec::new(),
        };
        let d = decide_rebase(&client, "o", "r", &planned, false, true)
            .await
            .unwrap();
        assert!(d.should_rebase);
        assert_eq!(d.reason, RebaseReason::Forced);
    }

    #[tokio::test]
    async fn decide_rebase_skips_when_any_pr_approved_and_no_conflict() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls/1/reviews"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"state": "APPROVED", "user": {"login": "alice"}},
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "mergeable": true,
                "mergeable_state": "clean",
            })))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let planned = Changes {
            locals: vec![local_change_with_pull(
                Action::Update,
                Some(json!({"number": 1})),
            )],
            orphans: Vec::new(),
        };
        let d = decide_rebase(&client, "o", "r", &planned, false, false)
            .await
            .unwrap();
        assert!(!d.should_rebase);
        assert_eq!(d.reason, RebaseReason::SkippedForApprovals);
        assert_eq!(d.approved_pulls.len(), 1);
    }

    #[tokio::test]
    async fn decide_rebase_overrides_approval_when_bottom_conflicts() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls/1/reviews"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"state": "APPROVED", "user": {"login": "alice"}},
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "mergeable": false,
                "mergeable_state": "dirty",
            })))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let planned = Changes {
            locals: vec![local_change_with_pull(
                Action::Update,
                Some(json!({"number": 1})),
            )],
            orphans: Vec::new(),
        };
        let d = decide_rebase(&client, "o", "r", &planned, false, false)
            .await
            .unwrap();
        assert!(d.should_rebase);
        assert_eq!(d.reason, RebaseReason::ConflictOverride);
    }

    #[tokio::test]
    async fn decide_rebase_rebases_when_no_approvals() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls/1/reviews"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "mergeable": true,
                "mergeable_state": "clean",
            })))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let planned = Changes {
            locals: vec![local_change_with_pull(
                Action::Update,
                Some(json!({"number": 1})),
            )],
            orphans: Vec::new(),
        };
        let d = decide_rebase(&client, "o", "r", &planned, false, false)
            .await
            .unwrap();
        assert!(d.should_rebase);
        assert_eq!(d.reason, RebaseReason::NoApprovals);
        assert!(d.approved_pulls.is_empty());
    }

    #[tokio::test]
    async fn decide_rebase_excludes_skip_merged_pulls_from_approval_set() {
        // A merged PR's approvals don't matter — its commit is
        // already on trunk and won't be force-pushed. The Python
        // implementation filters skip-merged out of the approval
        // check; if we ever regress, a stack with a merged
        // approved PR followed by a fresh-create-with-no-PR
        // would falsely skip the rebase.
        let server = MockServer::start().await;
        // Only PR 2 should be queried — PR 1 is skip-merged.
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls/2/reviews"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls/2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "mergeable": true,
                "mergeable_state": "clean",
            })))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let planned = Changes {
            locals: vec![
                local_change_with_pull(Action::SkipMerged, Some(json!({"number": 1}))),
                local_change_with_pull(Action::Update, Some(json!({"number": 2}))),
            ],
            orphans: Vec::new(),
        };
        let d = decide_rebase(&client, "o", "r", &planned, false, false)
            .await
            .unwrap();
        // No approvals on the live PR, so rebase happens.
        assert!(d.should_rebase);
        assert_eq!(d.reason, RebaseReason::NoApprovals);
    }
}
