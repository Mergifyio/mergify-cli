//! Discover the open + recently merged PRs belonging to a stack.
//!
//! Every `mergify stack <cmd>` that needs to know which PRs are
//! already on GitHub for the current branch runs this. It only
//! calls repository-scoped endpoints (`/repos/{owner}/{repo}/...`):
//! some environments proxy the GitHub API and refuse everything
//! else (Claude Code on the web answers `/search/issues` with a
//! 403, Mergifyio/mergify-cli#1861), and the Search API has its
//! own small rate limit on top.
//!
//! 1. `GET /repos/owner/repo/git/matching-refs/heads/<prefix>/` —
//!    every branch that exists under the stack prefix, in one
//!    unpaginated call. Kept: branches one segment below the prefix
//!    whose last segment carries a Change-Id, i.e. the shape
//!    `stack push` creates.
//! 2. Add the branch each local commit would be pushed to
//!    (`<prefix>/<slug>`) when no live branch carries its
//!    Change-Id. That is how a merged PR is still found after its
//!    head branch was deleted: `SkipMerged` needs the PR's
//!    `head.sha` to equal the local commit, and a commit that
//!    unchanged has the same title, so the same slug. A commit
//!    reworded since has a different SHA and is re-created
//!    whether its merged PR is found or not.
//! 3. For each of those branches,
//!    `GET /repos/owner/repo/pulls?head=owner:<branch>&state=all`.
//!    That filter reads the PR's recorded head, so it answers for
//!    closed and merged PRs whose branch is gone.
//! 4. Sort everything by `updated_at`, newest first, and group by
//!    [`change_id::extract_from_branch_segment`] applied to the
//!    last segment of `head.ref`. Closed-but-not-merged PRs are
//!    dropped. When two PRs share the same Change-Id, open beats
//!    closed — and two open PRs on the same Change-Id is a hard
//!    error (the user has a duplicate that the rest of the
//!    orchestration can't reconcile).
//!
//! The payloads are the pull-request *list* shape: everything the
//! single-PR endpoint returns except the computed fields
//! (`mergeable`, `mergeable_state`, `merged`, diff stats). A
//! consumer that needs one of those fetches the PR itself.
//!
//! What the Search API found and this does not: a merged PR whose
//! branch was deleted and whose Change-Id is no longer in the local
//! stack. Consumers only report *open* orphans, so nothing reads it.
//!
//! The output is order-preserving — the `updated_at` ordering
//! carries through to downstream consumers (the orphan list is
//! presented in that order).

use mergify_core::http::Client;
use mergify_core::{ApiFlavor, CliError};
use serde::{Deserialize, Serialize};

use crate::change_id;
use crate::local_commits::LocalCommit;

/// One `{change_id, pull}` entry, most recently updated first.
/// Emitted as a flat array (not a JSON object) so consumers can
/// rebuild an order-preserving dict without relying on serde's
/// optional `preserve_order` feature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteChange {
    pub change_id: String,
    /// Raw PR payload from `/repos/owner/repo/pulls?head=...` (the
    /// list shape — no `mergeable`). Passed through as a typeless
    /// JSON value — the downstream orchestrator consumes many
    /// fields (`head.ref`, `head.sha`, `state`, `draft`,
    /// `merged_at`, `merge_commit_sha`, `html_url`, `stack`, …);
    /// typing them all here would be a translation tax with no
    /// real correctness win.
    pub pull: serde_json::Value,
}

/// Run the list-branches → look-up → group pipeline.
///
/// `user` / `repo` name the repository the stack lives in.
/// `stack_prefix` is the branch prefix the stack's PR branches sit
/// under. `author` keeps only PRs one user opened — `Some(me)` for
/// the commands that only ever manage the local user's own stack,
/// `None` for `stack checkout`, which has to reach anyone's stack.
/// `local_commits` supplies the branch names a merged PR may still
/// be recorded under after its branch was deleted; pass `&[]` when
/// there is no local stack (then only live branches are searched).
pub async fn get_remote_changes(
    client: &Client,
    user: &str,
    repo: &str,
    stack_prefix: &str,
    author: Option<&str>,
    local_commits: &[LocalCommit],
) -> Result<Vec<RemoteChange>, CliError> {
    get_remote_changes_reporting(
        client,
        user,
        repo,
        stack_prefix,
        author,
        local_commits,
        |_, _| {},
    )
    .await
}

/// Same as [`get_remote_changes`], but calls `on_lookup(n, total)`
/// just before the `n`-th (1-based) of `total` per-branch lookups —
/// so a caller can surface progress through the otherwise-silent
/// sequential loop.
pub async fn get_remote_changes_reporting(
    client: &Client,
    user: &str,
    repo: &str,
    stack_prefix: &str,
    author: Option<&str>,
    local_commits: &[LocalCommit],
    mut on_lookup: impl FnMut(usize, usize),
) -> Result<Vec<RemoteChange>, CliError> {
    let live = live_stack_branches(client, user, repo, stack_prefix).await?;
    let branches = branches_to_look_up(live, stack_prefix, local_commits);

    // Sequential on purpose: GitHub's secondary rate limit kicks in
    // on bursts of concurrent requests, and a stack is a handful of
    // branches (~100ms each).
    let pulls_path = format!("/repos/{user}/{repo}/pulls");
    let mut pulls: Vec<serde_json::Value> = Vec::new();
    for (idx, branch) in branches.iter().enumerate() {
        on_lookup(idx + 1, branches.len());
        let head = format!("{user}:{branch}");
        let found: Vec<serde_json::Value> = client
            .get_with_query(
                &pulls_path,
                &[("head", &head), ("state", "all"), ("per_page", "100")],
            )
            .await?;
        pulls.extend(
            found
                .into_iter()
                .filter(|pull| author.is_none_or(|a| opened_by(pull, a))),
        );
    }

    // ISO-8601 UTC timestamps sort chronologically as strings. A
    // stable sort keeps lookup order between equal timestamps.
    pulls.sort_by(|a, b| updated_at(b).cmp(updated_at(a)));
    group_by_change_id(pulls)
}

/// Branches that exist right now one segment below `stack_prefix`
/// and end in a Change-Id — the branches `stack push` creates.
/// A nested stack (`<prefix>/<sub>/...`) or a hand-made branch
/// under the prefix is not this stack's.
async fn live_stack_branches(
    client: &Client,
    user: &str,
    repo: &str,
    stack_prefix: &str,
) -> Result<Vec<String>, CliError> {
    // The trailing slash matters: `matching-refs` is a plain string
    // prefix match, so without it `stack/foo` would also list
    // `stack/foobar/...`.
    let path = format!("/repos/{user}/{repo}/git/matching-refs/heads/{stack_prefix}/");
    let refs: Vec<GitRef> = client.get(&path).await?;
    let ref_prefix = format!("refs/heads/{stack_prefix}/");
    Ok(refs
        .into_iter()
        .filter_map(|r| {
            let leaf = r.ref_name.strip_prefix(&ref_prefix)?;
            let managed =
                !leaf.contains('/') && change_id::extract_from_branch_segment(leaf).is_some();
            managed.then(|| format!("{stack_prefix}/{leaf}"))
        })
        .collect())
}

/// `live`, plus the branch each local commit would be pushed to
/// when no live branch already carries its Change-Id.
fn branches_to_look_up(
    live: Vec<String>,
    stack_prefix: &str,
    local_commits: &[LocalCommit],
) -> Vec<String> {
    let live_change_ids: Vec<String> = live
        .iter()
        .filter_map(|b| change_id::extract_from_branch_segment(last_segment(b)))
        .map(str::to_owned)
        .collect();
    let mut branches = live;
    for local in local_commits {
        if live_change_ids
            .iter()
            .any(|remote| same_change(remote, &local.change_id))
        {
            continue;
        }
        let expected = format!("{stack_prefix}/{slug}", slug = local.slug);
        if !branches.contains(&expected) {
            branches.push(expected);
        }
    }
    branches
}

/// Whether a branch's Change-Id (the short 8-hex form or the full
/// `I` + 40 hex) and a local commit's full Change-Id name the same
/// change. The same prefix comparison the classifiers pair commits
/// with PRs by.
fn same_change(remote: &str, local: &str) -> bool {
    let remote_hex = remote.strip_prefix('I').unwrap_or(remote);
    let local_hex = local.strip_prefix('I').unwrap_or(local);
    remote_hex.starts_with(local_hex) || local_hex.starts_with(remote_hex)
}

fn last_segment(branch: &str) -> &str {
    branch.rsplit('/').next().unwrap_or(branch)
}

/// GitHub logins are case-insensitive, as the Search API's
/// `author:` qualifier this replaces was.
fn opened_by(pull: &serde_json::Value, author: &str) -> bool {
    pull.pointer("/user/login")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|login| login.eq_ignore_ascii_case(author))
}

fn updated_at(pull: &serde_json::Value) -> &str {
    pull.get("updated_at")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

/// Pure, network-free regrouping of the per-PR payloads. Exposed
/// so the lookup + group split can be unit-tested in isolation
/// without spinning up a mock server for every parser edge case.
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
        let Some(change_id) = change_id::extract_from_branch_segment(last_segment(head_ref)) else {
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
                    return Err(CliError::InvalidState(format!(
                        "More than 1 pull found with this head: {head_ref}"
                    )));
                }
                // Open-existing + closed-new, or both closed:
                // keep the existing entry. The input is sorted by
                // `updated_at`, newest first, so the first-seen
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
struct GitRef {
    #[serde(rename = "ref")]
    ref_name: String,
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
    use serde_json::{Value, json};
    use url::Url;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
        // without going through `mergify stack`, and opened a PR
        // on it. Dropping it keeps the result hermetic to managed
        // PRs.
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
        // The lookup sorts by `updated_at` before grouping, and
        // the orphan list observes that order. Confirm grouping
        // doesn't silently re-sort it.
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
        // close one of the duplicates manually.
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

    const ID_A: &str = "Iaaaaaaaa0000000000000000000000000000000a";
    const ID_B: &str = "Ibbbbbbbb0000000000000000000000000000000b";

    fn local(change_id: &str, slug: &str) -> LocalCommit {
        LocalCommit {
            commit_sha: format!("sha-{slug}"),
            title: slug.to_string(),
            message: String::new(),
            change_id: change_id.to_string(),
            slug: slug.to_string(),
            note: String::new(),
        }
    }

    /// A list-endpoint PR payload, with the fields the lookup
    /// filters and sorts on.
    fn listed(number: u64, state: &str, head_ref: &str, login: &str, updated: &str) -> Value {
        let mut pull = pr(number, state, head_ref, None);
        pull["user"] = json!({ "login": login });
        pull["updated_at"] = json!(updated);
        pull
    }

    fn refs(names: &[&str]) -> Value {
        Value::Array(
            names
                .iter()
                .map(|n| json!({ "ref": format!("refs/heads/{n}"), "object": { "sha": "x" } }))
                .collect(),
        )
    }

    async fn mount_refs(server: &MockServer, prefix: &str, names: &[&str]) {
        Mock::given(method("GET"))
            .and(path(format!(
                "/repos/user/repo/git/matching-refs/heads/{prefix}/"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(refs(names)))
            .expect(1)
            .mount(server)
            .await;
    }

    async fn mount_lookup(server: &MockServer, branch: &str, pulls: Value) {
        Mock::given(method("GET"))
            .and(path("/repos/user/repo/pulls"))
            .and(query_param("head", format!("user:{branch}")))
            .and(query_param("state", "all"))
            .and(query_param("per_page", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(pulls))
            .expect(1)
            .mount(server)
            .await;
    }

    /// The endpoint this module used to call. Mounted with
    /// `expect(0)` so any regression back to it fails the test.
    async fn forbid_search(server: &MockServer) {
        Mock::given(path("/search/issues"))
            .respond_with(ResponseTemplate::new(403))
            .expect(0)
            .mount(server)
            .await;
    }

    fn client(server: &MockServer) -> Client {
        Client::new(
            Url::parse(&server.uri()).unwrap(),
            "tok".to_string(),
            ApiFlavor::GitHub,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn looks_up_each_live_stack_branch_without_the_search_api() {
        let server = MockServer::start().await;
        forbid_search(&server).await;
        mount_refs(
            &server,
            "prefix",
            &["prefix/feat-a--aaaaaaaa", "prefix/feat-b--bbbbbbbb"],
        )
        .await;
        mount_lookup(
            &server,
            "prefix/feat-a--aaaaaaaa",
            json!([listed(
                11,
                "open",
                "prefix/feat-a--aaaaaaaa",
                "author",
                "2026-01-01T00:00:00Z"
            )]),
        )
        .await;
        mount_lookup(
            &server,
            "prefix/feat-b--bbbbbbbb",
            json!([listed(
                22,
                "open",
                "prefix/feat-b--bbbbbbbb",
                "author",
                "2026-01-02T00:00:00Z"
            )]),
        )
        .await;

        let mut progress = Vec::new();
        let got = get_remote_changes_reporting(
            &client(&server),
            "user",
            "repo",
            "prefix",
            Some("author"),
            &[],
            |n, total| progress.push((n, total)),
        )
        .await
        .unwrap();

        // Most recently updated first, as the Search API's
        // `sort=updated` used to return them.
        assert_eq!(
            got.iter()
                .map(|c| (c.change_id.as_str(), c.pull["number"].as_u64()))
                .collect::<Vec<_>>(),
            vec![("bbbbbbbb", Some(22)), ("aaaaaaaa", Some(11))],
        );
        assert_eq!(progress, vec![(1, 2), (2, 2)]);
    }

    #[tokio::test]
    async fn finds_a_merged_pull_whose_branch_was_deleted_through_the_local_slug() {
        // The bottom PR merged and GitHub deleted its branch, so
        // `matching-refs` no longer lists it. `stack sync` still
        // has to see that PR as merged to drop the local commit.
        let server = MockServer::start().await;
        forbid_search(&server).await;
        mount_refs(&server, "prefix", &["prefix/feat-b--bbbbbbbb"]).await;
        mount_lookup(
            &server,
            "prefix/feat-b--bbbbbbbb",
            json!([listed(
                22,
                "open",
                "prefix/feat-b--bbbbbbbb",
                "author",
                "2026-01-02T00:00:00Z"
            )]),
        )
        .await;
        let mut merged = listed(
            11,
            "closed",
            "prefix/feat-a--aaaaaaaa",
            "author",
            "2026-01-01T00:00:00Z",
        );
        merged["merged_at"] = json!("2026-01-01T00:00:00Z");
        mount_lookup(&server, "prefix/feat-a--aaaaaaaa", json!([merged])).await;

        let locals = [
            local(ID_A, "feat-a--aaaaaaaa"),
            local(ID_B, "feat-b--bbbbbbbb"),
        ];
        let got = get_remote_changes(
            &client(&server),
            "user",
            "repo",
            "prefix",
            Some("author"),
            &locals,
        )
        .await
        .unwrap();

        let merged = got
            .iter()
            .find(|c| c.change_id == "aaaaaaaa")
            .expect("merged pull found");
        assert_eq!(merged.pull["number"], 11);
        assert_eq!(merged.pull["merged_at"], "2026-01-01T00:00:00Z");
        assert_eq!(got.len(), 2);
    }

    #[tokio::test]
    async fn skips_the_slug_lookup_for_a_change_already_on_a_live_branch() {
        // The commit was reworded after its PR opened: the PR keeps
        // its original branch, the local slug now differs. The live
        // branch already answers for that Change-Id, so the
        // reworded slug is not looked up.
        let server = MockServer::start().await;
        forbid_search(&server).await;
        mount_refs(&server, "prefix", &["prefix/old-title--aaaaaaaa"]).await;
        mount_lookup(
            &server,
            "prefix/old-title--aaaaaaaa",
            json!([listed(
                11,
                "open",
                "prefix/old-title--aaaaaaaa",
                "author",
                "2026-01-01T00:00:00Z"
            )]),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/repos/user/repo/pulls"))
            .and(query_param("head", "user:prefix/new-title--aaaaaaaa"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(0)
            .mount(&server)
            .await;

        let got = get_remote_changes(
            &client(&server),
            "user",
            "repo",
            "prefix",
            Some("author"),
            &[local(ID_A, "new-title--aaaaaaaa")],
        )
        .await
        .unwrap();

        assert_eq!(got.len(), 1);
        assert_eq!(got[0].pull["number"], 11);
    }

    #[tokio::test]
    async fn ignores_nested_and_unmanaged_branches_under_the_prefix() {
        // `prefix/sub/...` is another stack; `prefix/random` was
        // never pushed by `mergify stack`. Neither is looked up.
        let server = MockServer::start().await;
        forbid_search(&server).await;
        mount_refs(
            &server,
            "prefix",
            &["prefix/sub/feat-x--cccccccc", "prefix/random-branch"],
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/repos/user/repo/pulls"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(0)
            .mount(&server)
            .await;

        let got = get_remote_changes(&client(&server), "user", "repo", "prefix", None, &[])
            .await
            .unwrap();

        assert!(got.is_empty(), "got: {got:?}");
    }

    #[tokio::test]
    async fn author_filter_is_case_insensitive_and_absent_without_an_author() {
        // `stack checkout` passes no author and must see anyone's
        // PR; the owner-only commands keep just their own, matching
        // the login the way GitHub does (case-insensitively).
        for (author, expected) in [
            (Some("AUTHOR"), vec![11]),
            (Some("someone-else"), vec![]),
            (None, vec![11]),
        ] {
            let server = MockServer::start().await;
            mount_refs(&server, "prefix", &["prefix/feat-a--aaaaaaaa"]).await;
            mount_lookup(
                &server,
                "prefix/feat-a--aaaaaaaa",
                json!([listed(
                    11,
                    "open",
                    "prefix/feat-a--aaaaaaaa",
                    "author",
                    "2026-01-01T00:00:00Z"
                )]),
            )
            .await;

            let got = get_remote_changes(&client(&server), "user", "repo", "prefix", author, &[])
                .await
                .unwrap();

            assert_eq!(
                got.iter()
                    .filter_map(|c| c.pull["number"].as_u64())
                    .collect::<Vec<_>>(),
                expected,
                "author {author:?}",
            );
        }
    }

    #[tokio::test]
    async fn keeps_the_most_recently_updated_of_two_merged_pulls_on_one_change() {
        // Both merged, one Change-Id (re-opened and merged twice):
        // the newest wins, whatever order the lookups ran in.
        let server = MockServer::start().await;
        mount_refs(&server, "prefix", &["prefix/feat-a--aaaaaaaa"]).await;
        let mut older = listed(
            11,
            "closed",
            "prefix/feat-a--aaaaaaaa",
            "author",
            "2026-01-01T00:00:00Z",
        );
        older["merged_at"] = json!("2026-01-01T00:00:00Z");
        let mut newer = listed(
            12,
            "closed",
            "prefix/feat-a--aaaaaaaa",
            "author",
            "2026-02-01T00:00:00Z",
        );
        newer["merged_at"] = json!("2026-02-01T00:00:00Z");
        mount_lookup(&server, "prefix/feat-a--aaaaaaaa", json!([older, newer])).await;

        let got = get_remote_changes(&client(&server), "user", "repo", "prefix", None, &[])
            .await
            .unwrap();

        assert_eq!(got.len(), 1);
        assert_eq!(got[0].pull["number"], 12);
    }
}
