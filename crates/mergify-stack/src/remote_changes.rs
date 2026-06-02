//! Discover the open + recently merged PRs belonging to a stack.
//!
//! Every `mergify stack <cmd>` that needs to know which PRs are
//! already on GitHub for the current branch runs this:
//!
//! 1. `GET /search/issues?q=repo:owner/repo author:author
//!    is:pull-request head:prefix/` — GitHub search, sorted by
//!    `updated`, capped at 100 results (the same `per_page` cap
//!    the Python CLI used). Only one search call regardless of
//!    stack depth.
//! 2. For each result, `GET /repos/owner/repo/pulls/<number>` to
//!    fetch the full PR payload (the search index returns a
//!    pared-down shape; downstream code needs `head.sha`,
//!    `merged_at`, `draft`, etc.).
//! 3. Group by [`change_id::extract_from_branch_segment`] applied
//!    to the last segment of `head.ref`. Closed-but-not-merged
//!    PRs are dropped. When two PRs share the same Change-Id,
//!    open beats closed — and two open PRs on the same Change-Id
//!    is a hard error (the user has a duplicate that the rest of
//!    the orchestration can't reconcile).
//!
//! The output is order-preserving — GitHub's `sort=updated`
//! ordering carries through to downstream consumers (the orphan
//! list in `changes.py`'s `get_changes` iterates this map and the
//! order matters for how orphan PRs are presented).
//!
//! Exposed via the hidden `_internal stack-remote-changes`
//! subcommand on the `mergify` binary as a no-op-from-Python's-
//! perspective: the Rust impl is callable but `changes.py` still
//! does the HTTP work itself today. Migration of the Python call
//! site lands in a follow-up PR — that change requires updating
//! roughly 50 `respx`-mocked tests across the `mergify_cli/tests/stack/`
//! tree to drive the bridge stub instead, which is too much
//! churn to bundle here.

use mergify_core::http::Client;
use mergify_core::{ApiFlavor, CliError};
use serde::{Deserialize, Serialize};

use crate::change_id;

/// One `{change_id, pull}` entry in the order GitHub returned it
/// from `/search/issues`. Emitted as a flat array (not a JSON
/// object) so consumers can rebuild an order-preserving dict
/// without relying on serde's optional `preserve_order` feature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteChange {
    pub change_id: String,
    /// Raw PR payload from `/repos/owner/repo/pulls/<number>`.
    /// Passed through as a typeless JSON value — the downstream
    /// Python orchestrator consumes many fields (`head.ref`,
    /// `head.sha`, `state`, `draft`, `merged_at`, `merge_commit_sha`,
    /// `html_url`, …); typing them all here would be a
    /// translation tax with no real correctness win.
    pub pull: serde_json::Value,
}

/// Run the search → fetch → group pipeline.
///
/// `user` / `repo` form the search scope (`repo:user/repo`).
/// `stack_prefix` filters the search to PRs whose head branch
/// starts with `prefix/`. `author` restricts the search to PRs
/// the current user opened (Mergify only manages PRs the local
/// user owns).
pub async fn get_remote_changes(
    client: &Client,
    user: &str,
    repo: &str,
    stack_prefix: &str,
    author: &str,
) -> Result<Vec<RemoteChange>, CliError> {
    let q = format!("repo:{user}/{repo} author:{author} is:pull-request head:{stack_prefix}/");
    let search: SearchResponse = client
        .get_with_query(
            "/search/issues",
            &[("q", &q), ("per_page", "100"), ("sort", "updated")],
        )
        .await?;

    // Per-PR fetches run sequentially. The Python orchestrator
    // uses `asyncio.gather` for fan-out, but the underlying
    // reqwest client we wrap doesn't memoize connections by
    // host the same way httpx does — and GitHub's secondary
    // rate-limit kicks in around ~80 concurrent requests on the
    // search-resource pool. Sequential keeps it under the
    // threshold and the latency budget is small (~50 PRs × ~100ms
    // each = a few seconds for the largest realistic stacks).
    let mut pulls: Vec<serde_json::Value> = Vec::with_capacity(search.items.len());
    for item in &search.items {
        let path = format!("/repos/{user}/{repo}/pulls/{number}", number = item.number);
        let pull: serde_json::Value = client.get(&path).await?;
        pulls.push(pull);
    }

    group_by_change_id(pulls)
}

/// Pure, network-free regrouping of the per-PR payloads. Exposed
/// so the search + fetch + group split can be unit-tested in
/// isolation without spinning up a mock server for every parser
/// edge case.
fn group_by_change_id(pulls: Vec<serde_json::Value>) -> Result<Vec<RemoteChange>, CliError> {
    let mut out: Vec<RemoteChange> = Vec::new();
    for pull in pulls {
        // Closed-but-not-merged PRs are dropped early — they
        // have no role in the stack discovery flow and keeping
        // them would only inflate the orphan list.
        let state = pull.get("state").and_then(serde_json::Value::as_str);
        let merged_at = pull.get("merged_at");
        if state == Some("closed") && merged_at.is_some_and(serde_json::Value::is_null) {
            continue;
        }

        let head_ref = pull
            .pointer("/head/ref")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                CliError::GitHubApi("PR payload missing required `head.ref` field".to_string())
            })?;
        let last_segment = head_ref.rsplit('/').next().unwrap_or(head_ref);
        let Some(change_id) = change_id::extract_from_branch_segment(last_segment) else {
            continue;
        };
        let change_id = change_id.to_string();

        if let Some(existing) = out.iter_mut().find(|c| c.change_id == change_id) {
            let other_state = existing
                .pull
                .get("state")
                .and_then(serde_json::Value::as_str);
            match (other_state, state) {
                (Some("closed"), Some("open")) => {
                    existing.pull = pull;
                }
                (Some("open"), Some("open")) => {
                    // Two open PRs on the same Change-Id is a
                    // user-state bug the rest of the
                    // orchestration can't reconcile (push would
                    // race, lease check would clobber). Surface
                    // loudly so the user closes one manually.
                    //
                    // (The Python implementation this was ported
                    // from checked for state `"opened"` — never
                    // the GitHub value — so this duplicate path
                    // was dead. The Rust port preserves the
                    // *intended* behaviour, not the latent bug.)
                    return Err(CliError::InvalidState(format!(
                        "More than 1 pull found with this head: {head_ref}"
                    )));
                }
                // Open-existing + closed-new, or both closed:
                // keep the existing entry. GitHub returned the
                // search sorted by `updated`, so the first-seen
                // one is the most recent.
                _ => {}
            }
        } else {
            out.push(RemoteChange { change_id, pull });
        }
    }
    Ok(out)
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    items: Vec<SearchItem>,
}

#[derive(Debug, Deserialize)]
struct SearchItem {
    number: u64,
}

/// Build an [`ApiFlavor::GitHub`] client pre-configured for the
/// given server + token. Convenience for the binary wrapper so
/// the `_internal stack-remote-changes` arm doesn't need to
/// re-implement the construction every call.
pub fn default_client(github_server: url::Url, token: &str) -> Result<Client, CliError> {
    Client::new(github_server, token.to_string(), ApiFlavor::GitHub)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(number: u64, state: &str, head_ref: &str, merged_at: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "number": number,
            "state": state,
            "draft": false,
            "merged_at": merged_at,
            "head": { "ref": head_ref, "sha": format!("sha{number}") },
        })
    }

    #[test]
    fn group_skips_closed_unmerged_pr() {
        // Closed-but-not-merged PRs are abandoned drafts that
        // never made it in; keeping them would force the orphan
        // list to surface stale entries.
        let pulls = vec![pr(1, "closed", "prefix/feat-a--aaaaaaaa", None)];
        let out = group_by_change_id(pulls).unwrap();
        assert!(out.is_empty(), "got: {out:?}");
    }

    #[test]
    fn group_keeps_closed_merged_pr() {
        // Closed + merged_at present → the PR landed; downstream
        // code uses it to mark the change as `skip-merged`.
        let pulls = vec![pr(
            1,
            "closed",
            "prefix/feat-a--aaaaaaaa",
            Some("2026-01-01T00:00:00Z"),
        )];
        let out = group_by_change_id(pulls).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].change_id, "aaaaaaaa");
    }

    #[test]
    fn group_extracts_change_id_from_new_style_short_suffix() {
        // New-style branches end in `--<8 hex>`; the change-id
        // helper returns the short hex tail rather than the full
        // I-prefixed form.
        let pulls = vec![pr(1, "open", "prefix/improve-thing--deadbeef", None)];
        let out = group_by_change_id(pulls).unwrap();
        assert_eq!(out[0].change_id, "deadbeef");
    }

    #[test]
    fn group_extracts_change_id_from_old_style_full_segment() {
        // Old-style: the entire last segment IS the Change-Id
        // (I + 40 hex). The helper returns it verbatim.
        let full = "I0123456789abcdef0123456789abcdef01234567";
        let pulls = vec![pr(1, "open", &format!("prefix/{full}"), None)];
        let out = group_by_change_id(pulls).unwrap();
        assert_eq!(out[0].change_id, full);
    }

    #[test]
    fn group_drops_pr_whose_branch_has_no_recognisable_change_id() {
        // Some user manually pushed a branch under the prefix
        // without going through `mergify stack`. The search query
        // can still surface it (the prefix substring matches);
        // dropping it keeps the result hermetic to managed PRs.
        let pulls = vec![pr(1, "open", "prefix/random-branch", None)];
        let out = group_by_change_id(pulls).unwrap();
        assert!(out.is_empty(), "got: {out:?}");
    }

    #[test]
    fn group_picks_open_over_closed_when_change_ids_collide() {
        // Common amend-after-merge pattern: an older closed PR
        // exists for a Change-Id that's been rebased and reopened
        // under a fresh branch. The open one is the live source
        // of truth.
        let pulls = vec![
            pr(1, "closed", "prefix/feat-a--deadbeef", None),
            pr(2, "open", "prefix/feat-a--deadbeef", None),
        ];
        let out = group_by_change_id(pulls).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].pull["number"], 2);
    }

    #[test]
    fn group_preserves_first_seen_order_across_distinct_change_ids() {
        // GitHub returns search results in `sort=updated`
        // order; the orphan loop in `get_changes` observes that
        // order. Confirm it's not silently re-sorted.
        let pulls = vec![
            pr(1, "open", "prefix/feat-a--aaaaaaaa", None),
            pr(2, "open", "prefix/feat-b--bbbbbbbb", None),
            pr(3, "open", "prefix/feat-c--cccccccc", None),
        ];
        let out = group_by_change_id(pulls).unwrap();
        assert_eq!(
            out.iter().map(|c| c.change_id.as_str()).collect::<Vec<_>>(),
            vec!["aaaaaaaa", "bbbbbbbb", "cccccccc"],
        );
    }

    #[test]
    fn group_errors_on_two_open_prs_with_same_change_id() {
        // This is a user-state bug the rest of the orchestration
        // can't reconcile — surface it loudly so the user can
        // close one of the duplicates manually. Both PRs use
        // GitHub's actual state value `"open"` (not `"opened"`)
        // so the test exercises the real production path; the
        // Python implementation this was ported from compared
        // against `"opened"` and never reached the error branch.
        let pulls = vec![
            pr(1, "open", "prefix/feat-a--deadbeef", None),
            pr(2, "open", "prefix/feat-a--deadbeef", None),
        ];
        let err = group_by_change_id(pulls).unwrap_err();
        match err {
            CliError::InvalidState(msg) => {
                assert!(msg.contains("More than 1 pull found"), "got: {msg}");
                assert!(msg.contains("prefix/feat-a--deadbeef"), "got: {msg}");
            }
            other => panic!("expected InvalidState, got: {other:?}"),
        }
    }

    #[test]
    fn group_errors_when_pr_payload_lacks_head_ref() {
        // Malformed PR response — GitHub guarantees `head.ref`
        // for pull requests, so an absent field means we got
        // back something we didn't expect (cache poison, custom
        // proxy, etc.). Better to error than to silently drop.
        let pulls = vec![serde_json::json!({
            "number": 1,
            "state": "open",
            "merged_at": null,
            "head": {},
        })];
        let err = group_by_change_id(pulls).unwrap_err();
        match err {
            CliError::GitHubApi(msg) => {
                assert!(msg.contains("head.ref"), "got: {msg}");
            }
            other => panic!("expected GitHubApi, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_remote_changes_searches_then_fetches_each_pull() {
        // End-to-end smoke: the search query gets composed
        // correctly, each result triggers a per-PR fetch, the
        // grouped output matches.
        use url::Url;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/search/issues"))
            .and(query_param(
                "q",
                "repo:user/repo author:author is:pull-request head:prefix/",
            ))
            .and(query_param("per_page", "100"))
            .and(query_param("sort", "updated"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [
                    { "number": 11 },
                    { "number": 22 },
                ],
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/user/repo/pulls/11"))
            .respond_with(ResponseTemplate::new(200).set_body_json(pr(
                11,
                "open",
                "prefix/feat-a--aaaaaaaa",
                None,
            )))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/user/repo/pulls/22"))
            .respond_with(ResponseTemplate::new(200).set_body_json(pr(
                22,
                "open",
                "prefix/feat-b--bbbbbbbb",
                None,
            )))
            .mount(&server)
            .await;

        let client = Client::new(
            Url::parse(&server.uri()).unwrap(),
            "tok".to_string(),
            ApiFlavor::GitHub,
        )
        .unwrap();
        let got = get_remote_changes(&client, "user", "repo", "prefix", "author")
            .await
            .unwrap();

        assert_eq!(got.len(), 2);
        assert_eq!(got[0].change_id, "aaaaaaaa");
        assert_eq!(got[0].pull["number"], 11);
        assert_eq!(got[1].change_id, "bbbbbbbb");
        assert_eq!(got[1].pull["number"], 22);
    }
}
