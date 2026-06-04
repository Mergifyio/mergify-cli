//! Per-PR upsert + orphan-branch teardown for `stack push`.
//!
//! - [`create_or_update_pr`] — the `Create` action `POST`s a
//!   fresh PR; `Update` `PATCH`es the existing one with refreshed
//!   `head`/`base`/`title`/`body`. Body always goes through
//!   [`crate::push_helpers::format_pull_description`] so the
//!   `Change-Id:` trailer is stripped and the rendered
//!   `Depends-On:` header points at the current predecessor PR.
//! - [`delete_orphan_branch`] — `DELETE
//!   /repos/<u>/<r>/git/refs/heads/<branch>` for orphan PRs the
//!   classifier flagged (open PR whose Change-Id is no longer in
//!   the local stack). Uses `delete_if_exists` so a concurrent
//!   teardown won't surface a 404 to the caller.
//!
//! Ported from
//! `mergify_cli/stack/push.py::{create_or_update_pr, delete_stack}`.

use mergify_core::{CliError, DeleteOutcome, HttpClient};
use serde::Serialize;
use serde_json::Value;

use crate::changes::Action;
use crate::push_helpers::format_pull_description;

/// Inputs to [`create_or_update_pr`]. Decoupled from
/// [`crate::changes::LocalChange`] because the orchestrator
/// computes `dest_branch` / `base_branch` separately during
/// planning and the upserter shouldn't depend on the full
/// classifier output.
#[derive(Debug, Clone)]
pub struct PrUpsertInput<'a> {
    pub action: Action,
    /// Commit title — used as the PR title when
    /// `keep_pull_request_title_and_body` is `false`.
    pub title: &'a str,
    /// Commit message body. Passed through
    /// `format_pull_description` to produce the PR body when
    /// `keep_pull_request_title_and_body` is `false`.
    pub message: &'a str,
    /// Remote branch name `mergify stack` pushed the commit to
    /// — becomes the PR `head`.
    pub dest_branch: &'a str,
    /// Branch the PR targets (the predecessor PR's `dest_branch`
    /// for non-bottom rows, the trunk branch for the bottom).
    pub base_branch: &'a str,
    /// Existing PR payload for `Update`; ignored for `Create`.
    /// `None` on `Update` is a programmer error and surfaces as
    /// `CliError::Generic` (matches Python's `RuntimeError`).
    pub pull: Option<&'a Value>,
    /// PR number of the predecessor PR — produces the trailing
    /// `Depends-On: #<n>` in the body. `None` for the bottom of
    /// the stack.
    pub depends_on_number: Option<u64>,
    pub create_as_draft: bool,
    /// When `true`, `Update` keeps the existing PR title and
    /// rewrites only the body (still re-running
    /// `format_pull_description` on the existing body so a
    /// stale `Depends-On:` gets re-chained). Mirrors the
    /// `--keep-pull-request-title-and-body` flag.
    pub keep_pull_request_title_and_body: bool,
}

#[derive(Serialize)]
struct UpdateBodyBoth<'a> {
    head: &'a str,
    base: &'a str,
    title: &'a str,
    body: String,
}

#[derive(Serialize)]
struct UpdateBodyKeepTitle<'a> {
    head: &'a str,
    base: &'a str,
    body: String,
}

#[derive(Serialize)]
struct CreateBody<'a> {
    title: &'a str,
    body: String,
    draft: bool,
    head: &'a str,
    base: &'a str,
}

/// Upsert the PR for `input.action` and return the PR payload.
///
/// `Update` returns the existing pull verbatim (Python does the
/// same; the PATCH response is ignored). `Create` returns the
/// freshly-created PR payload from the POST response so the
/// orchestrator can stash the number for downstream `Depends-On:`
/// links.
///
/// `SkipMerged` / `SkipUpToDate` are *not* valid inputs — the
/// orchestrator filters them out before calling. Passing one
/// surfaces as `CliError::Generic` (matches Python's
/// `RuntimeError("Unhandled action: ...")`).
pub async fn create_or_update_pr(
    client: &HttpClient,
    user: &str,
    repo: &str,
    input: PrUpsertInput<'_>,
) -> Result<Value, CliError> {
    match input.action {
        Action::Update => {
            let pull = input.pull.ok_or_else(|| {
                CliError::Generic("Can't update pull with change.pull unset".to_string())
            })?;
            let number = pull
                .get("number")
                .and_then(Value::as_u64)
                .ok_or_else(|| CliError::Generic("update pull payload missing `number`".into()))?;
            let path = format!("/repos/{user}/{repo}/pulls/{number}");

            // Two PATCH body shapes for the same endpoint: when
            // `keep_pull_request_title_and_body` is true we want
            // GitHub to leave `title` alone, so we just don't
            // include the key. Sending `title: null` would
            // actually try to clear it — different from "don't
            // touch."
            if input.keep_pull_request_title_and_body {
                let existing_body = pull.get("body").and_then(Value::as_str).unwrap_or("");
                let body = UpdateBodyKeepTitle {
                    head: input.dest_branch,
                    base: input.base_branch,
                    body: format_pull_description(existing_body, input.depends_on_number),
                };
                let _: Value = client.patch(&path, &body).await?;
            } else {
                let body = UpdateBodyBoth {
                    head: input.dest_branch,
                    base: input.base_branch,
                    title: input.title,
                    body: format_pull_description(input.message, input.depends_on_number),
                };
                let _: Value = client.patch(&path, &body).await?;
            }

            Ok(pull.clone())
        }
        Action::Create => {
            let body = CreateBody {
                title: input.title,
                body: format_pull_description(input.message, input.depends_on_number),
                draft: input.create_as_draft,
                head: input.dest_branch,
                base: input.base_branch,
            };
            let path = format!("/repos/{user}/{repo}/pulls");
            let pull: Value = client.post(&path, &body).await?;
            Ok(pull)
        }
        Action::SkipMerged | Action::SkipUpToDate => Err(CliError::Generic(format!(
            "Unhandled action: {:?}",
            input.action,
        ))),
    }
}

/// Delete the remote branch for an orphan PR.
///
/// Orphan PRs are open PRs whose `Change-Id` is no longer in the
/// local stack — typically because the user dropped a commit
/// locally without closing the PR. Tearing them down keeps the
/// stack consistent with the local series. 404s are swallowed
/// (matches `delete_if_exists` semantics) so a concurrent
/// teardown by another tool doesn't surface as an error.
pub async fn delete_orphan_branch(
    client: &HttpClient,
    user: &str,
    repo: &str,
    branch_ref: &str,
) -> Result<DeleteOutcome, CliError> {
    let path = format!("/repos/{user}/{repo}/git/refs/heads/{branch_ref}");
    client.delete_if_exists(&path).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use mergify_core::{ApiFlavor, HttpClient};
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

    #[tokio::test]
    async fn create_posts_pulls_endpoint_with_draft_and_full_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(wm_path("/repos/o/r/pulls"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"number": 42})))
            .mount(&server)
            .await;

        let input = PrUpsertInput {
            action: Action::Create,
            title: "feat: x",
            message: "feat: x\n\nbody\n\nChange-Id: Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
            dest_branch: "jd/feature/Iaaaaaa",
            base_branch: "main",
            pull: None,
            depends_on_number: Some(7),
            create_as_draft: true,
            keep_pull_request_title_and_body: false,
        };
        let pull = create_or_update_pr(&client(&server), "o", "r", input)
            .await
            .unwrap();
        assert_eq!(pull["number"], 42);

        let body = request_body(&server.received_requests().await.unwrap()[0]);
        assert_eq!(body["title"], "feat: x");
        assert_eq!(body["draft"], true);
        assert_eq!(body["head"], "jd/feature/Iaaaaaa");
        assert_eq!(body["base"], "main");
        // Description has Change-Id stripped and predecessor PR
        // appended as Depends-On.
        let body_str = body["body"].as_str().unwrap();
        assert!(!body_str.contains("Change-Id"));
        assert!(body_str.ends_with("\n\nDepends-On: #7"));
    }

    #[tokio::test]
    async fn update_patches_pulls_endpoint_and_returns_existing_pull() {
        let server = MockServer::start().await;
        let existing = json!({
            "number": 42,
            "body": "old body\n\nDepends-On: #999",
        });
        Mock::given(method("PATCH"))
            .and(wm_path("/repos/o/r/pulls/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
            .mount(&server)
            .await;

        let input = PrUpsertInput {
            action: Action::Update,
            title: "feat: x (rewritten)",
            message: "feat: x\n\nfresh body",
            dest_branch: "jd/feature/Iaaaaaa",
            base_branch: "main",
            pull: Some(&existing),
            depends_on_number: None,
            create_as_draft: false,
            keep_pull_request_title_and_body: false,
        };
        let pull = create_or_update_pr(&client(&server), "o", "r", input)
            .await
            .unwrap();
        // Update returns the input pull verbatim — Python pins
        // this so downstream code can use it as-is without
        // re-reading the PATCH response.
        assert_eq!(pull, existing);

        let body = request_body(&server.received_requests().await.unwrap()[0]);
        assert_eq!(body["title"], "feat: x (rewritten)");
        assert_eq!(body["head"], "jd/feature/Iaaaaaa");
        assert_eq!(body["base"], "main");
        // Body is the commit message, not the old PR body, when
        // keep_pull_request_title_and_body is false.
        assert_eq!(body["body"], "feat: x\n\nfresh body");
    }

    #[tokio::test]
    async fn update_with_keep_title_omits_title_and_rewrites_body_from_existing() {
        // The existing PR body's `Depends-On: #999` gets rewritten
        // through `format_pull_description` so the stale predecessor
        // doesn't survive. With `depends_on_number: None` the new
        // body has no Depends-On at all.
        let server = MockServer::start().await;
        let existing = json!({
            "number": 42,
            "body": "old body\n\nDepends-On: #999",
        });
        Mock::given(method("PATCH"))
            .and(wm_path("/repos/o/r/pulls/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;

        let input = PrUpsertInput {
            action: Action::Update,
            title: "ignored",
            message: "ignored",
            dest_branch: "jd/feature/Iaaaaaa",
            base_branch: "main",
            pull: Some(&existing),
            depends_on_number: None,
            create_as_draft: false,
            keep_pull_request_title_and_body: true,
        };
        create_or_update_pr(&client(&server), "o", "r", input)
            .await
            .unwrap();

        let body = request_body(&server.received_requests().await.unwrap()[0]);
        // `title` key absent — GitHub interprets that as "don't
        // touch", which is what `--keep-pull-request-title-and-body`
        // means.
        assert!(body.get("title").is_none(), "title must be absent");
        let new_body = body["body"].as_str().unwrap();
        assert!(!new_body.contains("Depends-On"));
        assert!(new_body.starts_with("old body"));
    }

    #[tokio::test]
    async fn update_without_existing_pull_errors() {
        let server = MockServer::start().await;
        let input = PrUpsertInput {
            action: Action::Update,
            title: "x",
            message: "x",
            dest_branch: "b",
            base_branch: "main",
            pull: None,
            depends_on_number: None,
            create_as_draft: false,
            keep_pull_request_title_and_body: false,
        };
        let err = create_or_update_pr(&client(&server), "o", "r", input)
            .await
            .unwrap_err();
        let CliError::Generic(msg) = err else {
            panic!("expected Generic");
        };
        assert!(msg.contains("change.pull unset"));
    }

    #[tokio::test]
    async fn skip_actions_are_rejected() {
        // SkipMerged / SkipUpToDate are filtered by the
        // orchestrator before the upserter runs; if one slips
        // through, surface a clear error rather than silently
        // doing nothing.
        for action in [Action::SkipMerged, Action::SkipUpToDate] {
            let server = MockServer::start().await;
            let input = PrUpsertInput {
                action,
                title: "x",
                message: "x",
                dest_branch: "b",
                base_branch: "main",
                pull: None,
                depends_on_number: None,
                create_as_draft: false,
                keep_pull_request_title_and_body: false,
            };
            assert!(
                create_or_update_pr(&client(&server), "o", "r", input)
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn delete_orphan_branch_swallows_404() {
        // Idempotent teardown: 404 → Ok(NotFound) so a concurrent
        // tool that already deleted the branch doesn't crash
        // our run.
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(wm_path("/repos/o/r/git/refs/heads/orphan-branch"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let outcome = delete_orphan_branch(&client(&server), "o", "r", "orphan-branch")
            .await
            .unwrap();
        assert_eq!(outcome, DeleteOutcome::NotFound);
    }

    #[tokio::test]
    async fn delete_orphan_branch_returns_deleted_on_2xx() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(wm_path("/repos/o/r/git/refs/heads/orphan-branch"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        let outcome = delete_orphan_branch(&client(&server), "o", "r", "orphan-branch")
            .await
            .unwrap();
        assert_eq!(outcome, DeleteOutcome::Deleted);
    }
}
