//! Per-PR upserters for the two sticky comments `stack push`
//! maintains:
//!
//! - [`update_stack_comment_for_pull`] — the "this PR is part
//!   of a stack" table (see [`crate::stack_comment`]). Skipped
//!   when the stack has only one PR — a single-row table would
//!   be noise.
//! - [`update_revision_history_for_pull`] — the "Revision
//!   history" table (see [`crate::revision_history`]). On
//!   every push, parses the existing comment's JSON marker,
//!   **appends** the new revision row, and `PATCH`es — so
//!   historic links are preserved verbatim while the new row
//!   gets fresh rendering.
//!
//! Both walk the issue comments once, match on the header
//! (`StackComment::is_stack_comment` / `RevisionHistoryComment::
//! is_revision_comment`), then choose between PATCH (existing
//! found, body changed), no-op (existing found, body unchanged),
//! and POST (no existing). The revision upserter has a third
//! branch: header matched but the marker JSON couldn't be parsed
//! — overwrite with a fresh initial comment so a corrupted
//! historic comment doesn't permanently block updates.
//!
//! Ported from
//! `mergify_cli/stack/push.py::{_update_comment_for_pull,
//! _update_revision_for_pull}`. The orchestration around them
//! (fan-out via `asyncio.Semaphore`, filter merged PRs out, …)
//! lives in the eventual `stack_push` orchestrator port — these
//! are pure single-PR helpers.

use chrono::{DateTime, Utc};
use mergify_core::{CliError, HttpClient};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::change_type::ChangeType;
use crate::revision_history::RevisionHistoryComment;
use crate::stack_comment::{self, StackEntry};

#[derive(Debug, Deserialize)]
struct Comment {
    url: String,
    body: String,
}

#[derive(Serialize)]
struct BodyOnly<'a> {
    body: &'a str,
}

/// GitHub's comments endpoint returns `url` as an absolute URL,
/// but the HTTP client rejects absolute URLs to prevent base-URL
/// hijacking. Extract the path so the client can re-join it
/// against its known base.
fn comment_path(absolute_url: &str) -> Result<String, CliError> {
    let parsed = Url::parse(absolute_url).map_err(|e| {
        CliError::GitHubApi(format!(
            "comment payload had unparseable `url`: {absolute_url:?} ({e})",
        ))
    })?;
    let path = parsed.path();
    if let Some(query) = parsed.query() {
        Ok(format!("{path}?{query}"))
    } else {
        Ok(path.to_string())
    }
}

/// Upsert the stack-comment for one PR.
///
/// `total_pulls` is the count of live PRs in the stack (i.e.
/// the number of `entries`). When it's 1, skip *creation* —
/// a single-row "stack" table is noise on a non-stacked PR.
/// Existing comments are still updated in place (matches
/// Python's `_update_comment_for_pull`): the early-exit only
/// gates the POST.
///
/// `pull_number` is the PR being decorated — drives both the
/// 👈 marker placement in the rendered table and the
/// `is_current` flag in the JSON marker.
pub async fn update_stack_comment_for_pull(
    client: &HttpClient,
    user: &str,
    repo: &str,
    pull_number: u64,
    entries: &[StackEntry],
    stack_id: &str,
    total_pulls: usize,
) -> Result<(), CliError> {
    let new_body = stack_comment::body(entries, pull_number, stack_id);

    let path = format!("/repos/{user}/{repo}/issues/{pull_number}/comments");
    let comments: Vec<Comment> = client.get(&path).await?;

    for comment in &comments {
        if stack_comment::is_stack_comment(&comment.body) {
            if comment.body != new_body {
                let _: Value = client
                    .patch(&comment_path(&comment.url)?, &BodyOnly { body: &new_body })
                    .await?;
            }
            return Ok(());
        }
    }

    // No existing comment. Skip when there's only one PR in the
    // stack — single-row stack table is noise.
    if total_pulls <= 1 {
        return Ok(());
    }

    let _: Value = client.post(&path, &BodyOnly { body: &new_body }).await?;
    Ok(())
}

/// Inputs to [`update_revision_history_for_pull`] — the per-row
/// fields the orchestrator resolves before calling. Decoupled
/// from [`crate::changes::LocalChange`] because `pull_head_sha`,
/// `reason`, `change_type`, and `replay_sha` are all computed
/// during the push (not at classifier time).
#[derive(Debug, Clone)]
pub struct RevisionInput<'a> {
    pub pull_number: u64,
    pub old_sha: &'a str,
    pub new_sha: &'a str,
    pub change_type: ChangeType,
    pub reason: &'a str,
    pub replay_sha: Option<&'a str>,
    pub timestamp: DateTime<Utc>,
}

/// Upsert the revision-history comment for one PR.
///
/// Three branches:
///
/// - **Existing parseable comment** → parse, [`append`] the new
///   row, render, PATCH if changed. Historic rows render
///   verbatim so old links stay intact.
/// - **Existing comment, header matches but marker corrupted** →
///   PATCH with a fresh 2-row `create_initial` comment.
///   Recovers from a hand-edited / out-of-schema marker
///   without leaving a stuck PR.
/// - **No existing comment** → POST a fresh `create_initial`.
///
/// [`append`]: crate::revision_history::RevisionHistoryComment::append
pub async fn update_revision_history_for_pull(
    client: &HttpClient,
    user: &str,
    repo: &str,
    github_server: &str,
    input: &RevisionInput<'_>,
) -> Result<(), CliError> {
    let path = format!(
        "/repos/{user}/{repo}/issues/{pull_number}/comments",
        pull_number = input.pull_number,
    );
    let comments: Vec<Comment> = client.get(&path).await?;

    // Cheap to construct unconditionally — both the
    // corrupted-marker and no-comment branches need it, and the
    // common parseable-comment branch ignores it.
    let fresh = RevisionHistoryComment::create_initial(
        github_server,
        user,
        repo,
        input.old_sha,
        input.new_sha,
        input.change_type,
        input.timestamp,
        input.reason,
        input.replay_sha,
    );

    for comment in &comments {
        if !RevisionHistoryComment::is_revision_comment(&comment.body) {
            continue;
        }
        match RevisionHistoryComment::parse(&comment.body, github_server, user, repo) {
            Some(mut parsed) => {
                parsed.append(
                    input.old_sha,
                    input.new_sha,
                    input.change_type,
                    input.timestamp,
                    input.reason,
                    input.replay_sha,
                );
                let new_body = parsed.body(input.pull_number);
                if comment.body != new_body {
                    let _: Value = client
                        .patch(&comment_path(&comment.url)?, &BodyOnly { body: &new_body })
                        .await?;
                }
            }
            None => {
                // Header matched but marker corrupted — recover
                // by overwriting with a fresh initial comment.
                // Better than leaving the PR with a stuck
                // unparseable comment.
                let _: Value = client
                    .patch(
                        &comment_path(&comment.url)?,
                        &BodyOnly {
                            body: &fresh.body(input.pull_number),
                        },
                    )
                    .await?;
            }
        }
        return Ok(());
    }

    // No existing revision-history comment — POST a fresh one.
    let _: Value = client
        .post(
            &path,
            &BodyOnly {
                body: &fresh.body(input.pull_number),
            },
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use mergify_core::ApiFlavor;
    use serde_json::json;
    use url::Url;
    use wiremock::matchers::{method, path as wm_path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    fn client(server: &MockServer) -> HttpClient {
        HttpClient::new(
            Url::parse(&server.uri()).unwrap(),
            "token",
            ApiFlavor::GitHub,
        )
        .unwrap()
    }

    fn request_body(req: &Request) -> Value {
        serde_json::from_slice(&req.body).expect("body is json")
    }

    fn entry(number: u64, change_id: &str) -> StackEntry {
        StackEntry {
            number,
            change_id: change_id.into(),
            head_sha: "1111111111111111111111111111111111111111".into(),
            base_branch: "main".into(),
            dest_branch: format!("jd/feature/{}", &change_id[..7]),
            title: format!("feat: change {number}"),
            html_url: format!("https://github.com/o/r/pull/{number}"),
        }
    }

    fn ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 4, 12, 30, 0).unwrap()
    }

    #[tokio::test]
    async fn stack_comment_creates_when_missing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(wm_path("/repos/o/r/issues/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(wm_path("/repos/o/r/issues/1/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({})))
            .mount(&server)
            .await;

        let entries = vec![
            entry(1, "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            entry(2, "Ibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        ];
        update_stack_comment_for_pull(&client(&server), "o", "r", 1, &entries, "feat", 2)
            .await
            .unwrap();

        let last = server.received_requests().await.unwrap();
        let post = last.iter().find(|r| r.method.as_str() == "POST").unwrap();
        let body = request_body(post);
        assert!(
            body["body"]
                .as_str()
                .unwrap()
                .starts_with("This pull request is part of a [Mergify stack]")
        );
    }

    #[tokio::test]
    async fn stack_comment_patches_when_existing_body_differs() {
        let server = MockServer::start().await;
        // The existing comment has our header — recognised as
        // ours — but stale content; the upserter must PATCH it
        // (not POST a duplicate).
        Mock::given(method("GET"))
            .and(wm_path("/repos/o/r/issues/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "url": format!("{}/repos/o/r/issues/comments/100", server.uri()),
                    "body": "This pull request is part of a [Mergify stack](https://docs.mergify.com/stacks/):\nSTALE",
                },
            ])))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(wm_path("/repos/o/r/issues/comments/100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;

        let entries = vec![
            entry(1, "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            entry(2, "Ibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        ];
        update_stack_comment_for_pull(&client(&server), "o", "r", 1, &entries, "feat", 2)
            .await
            .unwrap();

        let reqs = server.received_requests().await.unwrap();
        let patch = reqs.iter().find(|r| r.method.as_str() == "PATCH").unwrap();
        let body = request_body(patch);
        // Fresh body has the marker and 👈 on row 1.
        assert!(
            body["body"]
                .as_str()
                .unwrap()
                .contains("<!-- mergify-stack-data: ")
        );
        assert!(body["body"].as_str().unwrap().contains("👈"));
    }

    #[tokio::test]
    async fn stack_comment_skips_when_only_one_pull() {
        // total_pulls == 1 → no POST. A single-row table for a
        // standalone PR is noise; the orchestrator skips the
        // upsert entirely.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(wm_path("/repos/o/r/issues/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        // No POST mock — if the code tried to POST, wiremock
        // would 404 and the call would fail.

        let entries = vec![entry(1, "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")];
        update_stack_comment_for_pull(&client(&server), "o", "r", 1, &entries, "feat", 1)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn revision_history_creates_when_missing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(wm_path("/repos/o/r/issues/42/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(wm_path("/repos/o/r/issues/42/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({})))
            .mount(&server)
            .await;

        let input = RevisionInput {
            pull_number: 42,
            old_sha: "aaaaaaaaaaaa",
            new_sha: "bbbbbbbbbbbb",
            change_type: ChangeType::Content,
            reason: "review feedback",
            replay_sha: None,
            timestamp: ts(),
        };
        update_revision_history_for_pull(
            &client(&server),
            "o",
            "r",
            "https://api.github.com",
            &input,
        )
        .await
        .unwrap();

        let reqs = server.received_requests().await.unwrap();
        let post = reqs.iter().find(|r| r.method.as_str() == "POST").unwrap();
        let body = request_body(post);
        let body_str = body["body"].as_str().unwrap();
        assert!(body_str.starts_with("### Revision history\n"));
        // First push → initial 2-row comment: synthetic "initial"
        // row + the actual revision.
        assert!(body_str.contains("| 1 | initial |"));
        assert!(body_str.contains("| 2 | content |"));
    }

    #[tokio::test]
    async fn revision_history_appends_to_existing_parseable_comment() {
        // Seed the API with a real 2-row comment produced by
        // `create_initial`. The upserter must parse it, append
        // the new row, and PATCH — historic row 1 + 2 stay
        // verbatim, fresh row 3 gets added.
        let seed = RevisionHistoryComment::create_initial(
            "https://api.github.com",
            "o",
            "r",
            "aaaaaaaaaaaa",
            "bbbbbbbbbbbb",
            ChangeType::Content,
            ts(),
            "first push",
            None,
        );
        let seed_body = seed.body(42);

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(wm_path("/repos/o/r/issues/42/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "url": format!("{}/repos/o/r/issues/comments/200", server.uri()),
                    "body": seed_body,
                },
            ])))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(wm_path("/repos/o/r/issues/comments/200"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;

        let input = RevisionInput {
            pull_number: 42,
            old_sha: "bbbbbbbbbbbb",
            new_sha: "cccccccccccc",
            change_type: ChangeType::Rebase,
            reason: "",
            replay_sha: None,
            timestamp: ts(),
        };
        update_revision_history_for_pull(
            &client(&server),
            "o",
            "r",
            "https://api.github.com",
            &input,
        )
        .await
        .unwrap();

        let reqs = server.received_requests().await.unwrap();
        let patch = reqs.iter().find(|r| r.method.as_str() == "PATCH").unwrap();
        let body = request_body(patch);
        let body_str = body["body"].as_str().unwrap();
        assert!(body_str.contains("| 1 | initial |"));
        assert!(body_str.contains("| 2 | content |"));
        // New rebase row appended.
        assert!(body_str.contains("| 3 | rebase |"));
    }

    #[tokio::test]
    async fn revision_history_overwrites_when_existing_marker_corrupted() {
        // The header matches but the marker JSON is junk —
        // parse() returns None. Recovery path: PATCH with a
        // fresh initial 2-row comment so the next push has a
        // clean slate.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(wm_path("/repos/o/r/issues/42/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "url": format!("{}/repos/o/r/issues/comments/300", server.uri()),
                    "body": "### Revision history\n\n<!-- mergify-revision-data: not-json -->",
                },
            ])))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(wm_path("/repos/o/r/issues/comments/300"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;

        let input = RevisionInput {
            pull_number: 42,
            old_sha: "aaaaaaaaaaaa",
            new_sha: "bbbbbbbbbbbb",
            change_type: ChangeType::Content,
            reason: "recover",
            replay_sha: None,
            timestamp: ts(),
        };
        update_revision_history_for_pull(
            &client(&server),
            "o",
            "r",
            "https://api.github.com",
            &input,
        )
        .await
        .unwrap();

        let reqs = server.received_requests().await.unwrap();
        let patch = reqs.iter().find(|r| r.method.as_str() == "PATCH").unwrap();
        let body = request_body(patch);
        let body_str = body["body"].as_str().unwrap();
        // Fresh `create_initial` shape, not an append.
        assert!(body_str.contains("| 1 | initial |"));
        assert!(body_str.contains("| 2 | content |"));
        assert!(!body_str.contains("| 3 |"));
    }
}
