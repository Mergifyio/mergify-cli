//! Opt-in registration of a pushed stack with GitHub's **native**
//! Stacks API (`stack push --github-native`).
//!
//! Native membership is *additive*: Change-Id identity, the branch
//! layout and the revision history stay ours. All this module does is
//! tell GitHub "these PRs, in this order, are one stack" so its UI and
//! its stack-aware merge path see what our stack comment already
//! describes.
//!
//! # What a live registration does and does not block
//!
//! Registration is not inert — it locks *one* thing. Measured against
//! the live API (2026-08-05); endpoint paths below are written relative
//! to `/repos/{owner}/{repo}`, as elsewhere in this crate:
//!
//! - `PATCH /pulls/{n}` fails with 422 *"Cannot change the base branch
//!   because the pull request is part of a stack"* whenever the `base`
//!   key is present **at all**, including when it is set to the value
//!   the PR already has. That is the whole lock, and it is why
//!   [`crate::pr_upsert::create_or_update_pr`] sends `base` only when
//!   the PR is really being retargeted. The same PATCH without the key
//!   — new `title`, new `body`, even a `head` that does not exist —
//!   succeeds while stacked, and the stack survives the force-push of
//!   its members' branches. So a push that only refreshes commits
//!   needs no dissolve at all.
//! - The 422 is **not atomic**: a body carrying `base` *and* `title`
//!   applies the title and rejects the base. Firing one blind and
//!   treating the error as "nothing happened" is not an option.
//! - Orphan teardown ([`crate::pr_upsert::delete_orphan_branch`])
//!   deletes a dropped change's head branch, and GitHub closes not only
//!   that PR but every PR still based on the deleted branch. Push is
//!   safe today only because step 9 retargets the survivors *before*
//!   step 11 deletes the branch — a retarget the lock above would
//!   block. A PR closed this way cannot be reopened (its base branch is
//!   gone) or retargeted (it is closed): it is permanently lost. And
//!   the dropped PR, closed but unmerged, stays a member of the stack,
//!   which then no longer describes anything real.
//! - `POST /stacks` on PRs that are already in an open stack fails with
//!   422 *"are already part of a stack"* — a fresh POST does not
//!   replace an existing registration, so *re-forming* requires an
//!   unstack first.
//!
//! # The three shapes of a push
//!
//! Hence [`crate::commands::push`] dissolves the stack for exactly the
//! pushes that need it, and nothing else:
//!
//! | push | stacks requests |
//! |---|---|
//! | refresh commits (amend, reword, force-push) | **none** |
//! | append a change on top | one [`append`] |
//! | retarget a PR, or tear down an orphan | [`unstack`] up front, [`register`] at the end |
//!
//! The fence, when it is needed, is the whole mutation stage rather
//! than one call site: the retarget can come from
//! [`crate::pr_upsert::neutralize_stale_bases`] *or* from the upsert
//! itself, and the orphan teardown at the end depends on the retarget
//! having landed. In between, the flow is byte-for-byte the flow that
//! runs with the flag off, which is what makes the failure mode benign
//! — an interrupted push leaves the stack merely unregistered, i.e.
//! exactly today's behaviour.
//!
//! # Failure policy — deliberately asymmetric
//!
//! - [`register`] and [`append`] **never fail the push.** A repo
//!   without the feature, an old GHES, a chain with a hole in it, or a
//!   stack that has shrunk below the 2-PR floor all just leave the PRs
//!   unregistered. That is the documented graceful degradation: the
//!   user gets today's stack.
//! - [`unstack`] failing **is** a hard error *before* the mutations. It
//!   is the one case where carrying on produces the unrecoverable state
//!   above, so the push stops before it can touch a single PR. (After
//!   the mutations, when it is only used to rebuild a stale
//!   registration, a failure is harmless and the caller swallows it.)

use mergify_core::{CliError, HttpClient};
use serde::Serialize;
use serde_json::Value;

/// Fewest PRs GitHub will accept in a stack. A `POST /stacks` with one
/// pull request is rejected with 422 *"2 items required; only 1 was
/// supplied"*, so a single-change stack stays a plain PR — and a
/// 2-member stack that loses a member dissolves rather than re-forming.
const MIN_STACK_SIZE: usize = 2;

#[derive(Serialize)]
struct CreateStack<'a> {
    /// Bottom-to-top. GitHub requires JSON integers here; strings are
    /// rejected with a schema 422.
    pull_requests: &'a [u64],
}

/// The stack number these PR payloads say they currently belong to.
///
/// GitHub puts a `stack` object on the PR payload on the **default**
/// API version, and [`crate::remote_changes`] already fetches every
/// PR in full — so the current registration costs no extra request.
/// The `number` (not the `id`) is the path key for [`unstack`].
///
/// Members of one stack all report the same number; the first one
/// found wins.
pub fn registered_number<'a>(pulls: impl IntoIterator<Item = &'a Value>) -> Option<u64> {
    pulls
        .into_iter()
        .find_map(|pull| pull.pointer("/stack/number").and_then(Value::as_u64))
}

/// Whether `pull` is already a member of stack `number`, read from the
/// same `stack` object [`registered_number`] uses.
pub fn is_member_of(pull: &Value, number: u64) -> bool {
    pull.pointer("/stack/number").and_then(Value::as_u64) == Some(number)
}

/// Dissolve stack `number` so the PRs can be mutated again.
///
/// All-or-nothing by design — the endpoint takes no body and there is
/// no way to drop a single member. A 404 means the stack is already
/// gone (a stale number, or a concurrent unstack) and counts as
/// success: the postcondition "these PRs are not in a stack" holds
/// either way.
///
/// # Errors
///
/// Any other failure is returned. Unlike [`register`], a failure here
/// must stop a push that has not started mutating yet: the caller is
/// about to issue the `PATCH`es that a live registration turns into an
/// unrecoverable teardown (see the module docs). The same call is also
/// used *after* the mutations to rebuild a registration [`append`]
/// could not extend — there the postcondition is only cosmetic, and the
/// caller ignores the error.
pub async fn unstack(
    client: &HttpClient,
    user: &str,
    repo: &str,
    number: u64,
) -> Result<(), CliError> {
    let path = format!("/repos/{user}/{repo}/stacks/{number}/unstack");
    match client.post_empty_if_exists(&path).await {
        Ok(()) => {
            tracing::debug!(stack = number, "dissolved GitHub stack");
            Ok(())
        }
        Err(e) => Err(CliError::wrap(
            format!(
                "could not dissolve GitHub stack #{number} before updating the pull requests \
                 (GitHub refuses to change a pull request's base branch while it is stacked). \
                 Retry, or unstack it by hand and push again",
            ),
            e,
        )),
    }
}

/// Register `pulls` (bottom-to-top) as one native stack, best effort.
///
/// Returns the new stack number, or `None` when the stack was not
/// registered — which is a routine, non-failing outcome:
///
/// - fewer than [`MIN_STACK_SIZE`] open PRs (a one-change stack is a
///   plain PR, and a shrunken 2-PR stack dissolves);
/// - the repository or GitHub deployment has no Stacks API (old GHES,
///   feature not enabled) — 404;
/// - the chain has a hole in it, e.g. under
///   `--only-update-existing-pulls` — 422.
///
/// None of these are worth failing a push that has already created,
/// updated and linked every PR correctly, so the error is logged at
/// debug and swallowed. The caller reports the outcome as a progress
/// row rather than an error.
pub async fn register(client: &HttpClient, user: &str, repo: &str, pulls: &[u64]) -> Option<u64> {
    if pulls.len() < MIN_STACK_SIZE {
        tracing::debug!(
            count = pulls.len(),
            "not registering a GitHub stack: below the 2 pull request minimum"
        );
        return None;
    }
    let path = format!("/repos/{user}/{repo}/stacks");
    let body = CreateStack {
        pull_requests: pulls,
    };
    match client.post::<_, Value>(&path, &body).await {
        Ok(stack) => {
            let number = stack.get("number").and_then(Value::as_u64);
            tracing::debug!(?number, ?pulls, "registered GitHub stack");
            number
        }
        Err(e) => {
            tracing::debug!(
                error = %e,
                "GitHub stack registration unavailable; leaving the pull requests unstacked"
            );
            None
        }
    }
}

/// Append `pulls` (bottom-to-top) on top of registered stack `number`,
/// best effort.
///
/// The incremental counterpart to [`register`]: `POST /stacks/{n}/add`
/// keeps the stack — its number, its webhooks, its existing members'
/// registration — and only extends it, so pushing a new change on top
/// of a stack costs one request instead of a dissolve plus a full
/// re-registration.
///
/// GitHub requires the first appended PR's `base` ref to be the current
/// top member's `head` ref; a list that does not chain on is rejected
/// with 422 *"Pull requests must form a stack, where each PR's base ref
/// is the previous PR's head ref"*. Everything else the API cannot do
/// — inserting in the middle, reordering, dropping a member — has no
/// endpoint at all and goes through [`unstack`] + [`register`].
///
/// Returns `false` when the append did not happen, on the same terms as
/// [`register`]: never an error, always a caller-visible outcome. The
/// caller's remedy is to rebuild the stack from scratch.
pub async fn append(
    client: &HttpClient,
    user: &str,
    repo: &str,
    number: u64,
    pulls: &[u64],
) -> bool {
    if pulls.is_empty() {
        return true;
    }
    let path = format!("/repos/{user}/{repo}/stacks/{number}/add");
    let body = CreateStack {
        pull_requests: pulls,
    };
    match client.post::<_, Value>(&path, &body).await {
        Ok(_) => {
            tracing::debug!(stack = number, ?pulls, "appended to GitHub stack");
            true
        }
        Err(e) => {
            tracing::debug!(
                error = %e,
                stack = number,
                "could not append to the GitHub stack; rebuilding it instead"
            );
            false
        }
    }
}

/// One open pull request of the stack we are about to describe, in
/// stack order, paired with whether GitHub's registration already
/// holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Member {
    pub number: u64,
    pub registered: bool,
}

/// The members GitHub is missing, when they are exactly a suffix of
/// `members` — the only difference [`append`] can express.
///
/// `Some([])` means the registration already describes the stack and
/// the push should issue no request at all. `None` means the difference
/// is something else — a new change landed *under* an existing one, or
/// a registered member is no longer in the stack — and the caller has
/// to dissolve and re-register.
///
/// Merged members need no special case: they are not in `members` at
/// all (GitHub keeps them in the stack and infers the merged prefix
/// itself), so a stack whose bottom just merged still reads as "already
/// correct".
#[must_use]
pub fn appendable_tail(members: &[Member]) -> Option<Vec<u64>> {
    let split = members.iter().take_while(|m| m.registered).count();
    if members[split..].iter().any(|m| m.registered) {
        // A registered member sits above an unregistered one: the new
        // change went into the middle, not on top.
        return None;
    }
    Some(members[split..].iter().map(|m| m.number).collect())
}

/// PR number of `pull` when it is an open, unmerged pull request —
/// i.e. one that belongs in a stack registration.
///
/// Merged members are excluded deliberately: a fresh `POST /stacks`
/// describes the stack that is still in flight, and GitHub derives the
/// merged prefix itself.
pub fn open_pull_number(pull: Option<&Value>) -> Option<u64> {
    let pull = pull?;
    if pull.get("merged_at").is_some_and(|v| !v.is_null()) {
        return None;
    }
    if pull.get("state").and_then(Value::as_str) == Some("closed") {
        return None;
    }
    pull.get("number").and_then(Value::as_u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mergify_core::{ApiFlavor, HttpClient};
    use serde_json::json;
    use url::Url;
    use wiremock::matchers::{body_json, method, path as wm_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> HttpClient {
        HttpClient::new(
            Url::parse(&server.uri()).unwrap(),
            "token",
            ApiFlavor::GitHub,
        )
        .unwrap()
    }

    #[test]
    fn registered_number_reads_the_stack_object_off_a_pull_payload() {
        // Membership rides along on the payload `remote_changes`
        // already fetches — pinning the pointer keeps us from
        // reintroducing a `GET /stacks` round-trip.
        let pulls = [
            json!({"number": 1, "stack": null}),
            json!({"number": 2, "stack": {"id": 162_170, "number": 7, "position": 1}}),
        ];
        // The path key is `number` (7), not `id` (162170).
        assert_eq!(registered_number(pulls.iter()), Some(7));
    }

    #[test]
    fn registered_number_is_none_when_nothing_is_stacked() {
        let pulls = [json!({"number": 1, "stack": null}), json!({"number": 2})];
        assert_eq!(registered_number(pulls.iter()), None);
    }

    #[test]
    fn open_pull_number_selects_only_live_members() {
        assert_eq!(
            open_pull_number(Some(
                &json!({"number": 5, "state": "open", "merged_at": null})
            )),
            Some(5)
        );
        // Merged members are GitHub's to infer, not ours to re-send.
        assert_eq!(
            open_pull_number(Some(
                &json!({"number": 5, "state": "closed", "merged_at": "2026-01-01T00:00:00Z"})
            )),
            None
        );
        assert_eq!(
            open_pull_number(Some(
                &json!({"number": 5, "state": "closed", "merged_at": null})
            )),
            None
        );
        assert_eq!(open_pull_number(None), None);
    }

    #[tokio::test]
    async fn register_posts_members_bottom_to_top_as_integers() {
        // GitHub rejects stringified numbers with a schema 422, and
        // order is the stack order — both are load-bearing, so assert
        // the exact body.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(wm_path("/repos/o/r/stacks"))
            .and(body_json(json!({"pull_requests": [9, 10, 11]})))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"number": 12})))
            .expect(1)
            .mount(&server)
            .await;

        let got = register(&client(&server), "o", "r", &[9, 10, 11]).await;
        assert_eq!(got, Some(12));
    }

    #[tokio::test]
    async fn register_below_the_floor_issues_no_request() {
        // A one-change stack is a plain PR. Wiremock with no mounted
        // mock 404s any call, so `Some(_)` here would mean we made one.
        let server = MockServer::start().await;
        assert_eq!(register(&client(&server), "o", "r", &[9]).await, None);
        assert_eq!(register(&client(&server), "o", "r", &[]).await, None);
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn register_degrades_silently_when_the_api_is_unavailable() {
        // 404 is an old GHES or a repo without the feature; 422 is a
        // chain with a hole in it. Neither may fail a push whose PRs
        // are already correct.
        for status in [404, 422, 403] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(wm_path("/repos/o/r/stacks"))
                .respond_with(ResponseTemplate::new(status).set_body_string("nope"))
                .mount(&server)
                .await;
            assert_eq!(
                register(&client(&server), "o", "r", &[9, 10]).await,
                None,
                "status {status} must degrade, not fail"
            );
        }
    }

    #[tokio::test]
    async fn register_tolerates_a_response_without_a_number() {
        // A proxy or a future API version returning a shape we don't
        // recognise still means "registered"; we just have nothing to
        // display. Must not panic or error.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(wm_path("/repos/o/r/stacks"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({})))
            .mount(&server)
            .await;
        assert_eq!(register(&client(&server), "o", "r", &[9, 10]).await, None);
    }

    #[test]
    fn is_member_of_matches_only_the_stack_we_read() {
        let pull = json!({"number": 1, "stack": {"number": 7, "position": 1}});
        assert!(is_member_of(&pull, 7));
        assert!(!is_member_of(&pull, 8));
        assert!(!is_member_of(&json!({"number": 1, "stack": null}), 7));
    }

    #[test]
    fn appendable_tail_is_empty_when_the_registration_is_already_right() {
        // The routine push: same members, same order. Nothing to send —
        // this is the case that used to cost an unstack + a re-register
        // on every single push.
        let members = [
            Member {
                number: 101,
                registered: true,
            },
            Member {
                number: 102,
                registered: true,
            },
        ];
        assert_eq!(appendable_tail(&members), Some(vec![]));
    }

    #[test]
    fn appendable_tail_returns_the_new_changes_on_top() {
        let members = [
            Member {
                number: 101,
                registered: true,
            },
            Member {
                number: 102,
                registered: true,
            },
            Member {
                number: 103,
                registered: false,
            },
            Member {
                number: 104,
                registered: false,
            },
        ];
        assert_eq!(appendable_tail(&members), Some(vec![103, 104]));
    }

    #[test]
    fn appendable_tail_refuses_a_change_inserted_under_a_member() {
        // A new change in the middle moves the bases of everything
        // above it — there is no endpoint for that, so the caller has
        // to dissolve and re-register.
        let members = [
            Member {
                number: 101,
                registered: true,
            },
            Member {
                number: 103,
                registered: false,
            },
            Member {
                number: 102,
                registered: true,
            },
        ];
        assert_eq!(appendable_tail(&members), None);
    }

    #[test]
    fn appendable_tail_of_an_all_new_stack_is_everything() {
        let members = [
            Member {
                number: 101,
                registered: false,
            },
            Member {
                number: 102,
                registered: false,
            },
        ];
        assert_eq!(appendable_tail(&members), Some(vec![101, 102]));
        assert_eq!(appendable_tail(&[]), Some(vec![]));
    }

    #[tokio::test]
    async fn append_posts_the_new_members_to_the_add_endpoint() {
        // Same body shape as `register` — integers, bottom-to-top — but
        // onto the existing stack, so its number and its members'
        // registration survive.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(wm_path("/repos/o/r/stacks/7/add"))
            .and(body_json(json!({"pull_requests": [11]})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"number": 7})))
            .expect(1)
            .mount(&server)
            .await;

        assert!(append(&client(&server), "o", "r", 7, &[11]).await);
    }

    #[tokio::test]
    async fn append_of_nothing_issues_no_request() {
        // The most common push of all: members unchanged. Wiremock
        // would 404 any call, and `true` here means we made none.
        let server = MockServer::start().await;
        assert!(append(&client(&server), "o", "r", 7, &[]).await);
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn append_reports_failure_instead_of_erroring() {
        // 422 (the appended PR doesn't chain onto the current top), 404
        // (someone dissolved the stack meanwhile). Both mean "rebuild
        // it", never "fail the push".
        for status in [404, 422, 409] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(wm_path("/repos/o/r/stacks/7/add"))
                .respond_with(ResponseTemplate::new(status).set_body_string("nope"))
                .mount(&server)
                .await;
            assert!(
                !append(&client(&server), "o", "r", 7, &[11]).await,
                "status {status} must report failure, not error"
            );
        }
    }

    #[tokio::test]
    async fn unstack_posts_to_the_stack_number_with_no_body() {
        // The endpoint takes no request body and returns an empty
        // 204 — decoding it as JSON would fail.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(wm_path("/repos/o/r/stacks/7/unstack"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        unstack(&client(&server), "o", "r", 7).await.unwrap();
    }

    #[tokio::test]
    async fn unstack_treats_404_as_already_dissolved() {
        // A stale stack number (someone unstacked by hand between our
        // fetch and now) satisfies the postcondition, so it must not
        // block the push.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(wm_path("/repos/o/r/stacks/7/unstack"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Not Found"))
            .mount(&server)
            .await;

        unstack(&client(&server), "o", "r", 7).await.unwrap();
    }

    #[tokio::test]
    async fn unstack_failure_is_fatal_and_explains_itself() {
        // The one asymmetry with `register`: carrying on past a failed
        // unstack is what permanently closes a surviving PR, so this
        // must stop the push before it mutates anything.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(wm_path("/repos/o/r/stacks/7/unstack"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;

        let err = unstack(&client(&server), "o", "r", 7).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("could not dissolve GitHub stack #7"),
            "got: {msg}"
        );
        assert!(msg.contains("unstack it by hand"), "got: {msg}");
    }
}
