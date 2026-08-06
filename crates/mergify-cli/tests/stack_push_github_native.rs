//! End-to-end tests for `mergify stack push --github-native`.
//!
//! Runs the real binary against a wiremock GitHub server and a real
//! git repo, and asserts on the *sequence* of requests it issued —
//! because the whole feature is a sequencing contract:
//!
//! - with the flag off, not a single `/stacks` request may be sent
//!   (the flag-off path has to stay exactly what it was);
//! - a push that only refreshes commits must leave the registration
//!   completely alone — no unstack, no re-registration, and no `base`
//!   in the PATCH bodies, which is what makes that possible;
//! - a push that adds changes on top must *extend* the stack rather
//!   than rebuild it;
//! - a push that retargets a pull request must `unstack` **before**
//!   the first PR mutation and `POST /stacks` **after** the last one.
//!   Getting that order wrong is what permanently closes a surviving
//!   pull request (see `mergify_stack::native_stack`), and no unit
//!   test on the module in isolation can catch it.

use std::path::{Path, PathBuf};
use std::process::Command;

use wiremock::matchers::{method, path as wm_path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn mergify_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mergify"))
}

fn isolated_git() -> Command {
    let mut cmd = Command::new("git");
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
    cmd.env("GIT_CONFIG_NOSYSTEM", "1");
    cmd
}

fn capture(dir: &Path, args: &[&str]) -> String {
    let out = isolated_git()
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn run_in(dir: &Path, args: &[&str]) {
    let ok = isolated_git()
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git -C {}: {args:?} failed", dir.display());
}

/// A `feature` branch with `n_commits` Change-Id-carrying commits on
/// top of a pushed `main`, plus a bare `origin` the push can reach.
fn build_stack_repo(n_commits: usize) -> (tempfile::TempDir, Vec<String>) {
    let workdir = tempfile::tempdir().unwrap();
    let upstream = workdir.path().join("up.git");
    isolated_git()
        .args([
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            upstream.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    let local = workdir.path().join("local");
    std::fs::create_dir(&local).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "t@e.com"],
        &["config", "user.name", "T"],
    ] {
        run_in(&local, args);
    }
    std::fs::write(local.join("root.txt"), "root").unwrap();
    run_in(&local, &["add", "root.txt"]);
    run_in(&local, &["commit", "-q", "-m", "root"]);
    run_in(
        &local,
        &["remote", "add", "origin", upstream.to_str().unwrap()],
    );
    run_in(&local, &["push", "-q", "origin", "main"]);
    run_in(&local, &["remote", "set-head", "origin", "main"]);
    run_in(&local, &["checkout", "-q", "-b", "feature"]);

    let mut change_ids = Vec::new();
    for i in 0..n_commits {
        let label = (b'A' + u8::try_from(i).expect("test stack stays under 26 commits")) as char;
        let fname = format!("{}.txt", label.to_lowercase());
        std::fs::write(local.join(&fname), format!("content {label}")).unwrap();
        run_in(&local, &["add", &fname]);
        // Distinct in the first 8 hex — that prefix is what a stack
        // branch segment carries, and a collision would make the two
        // commits look like one change.
        let cid = format!("I{:08x}{}", i + 1, "0".repeat(32));
        run_in(
            &local,
            &[
                "commit",
                "-q",
                "-m",
                &format!("Commit {label}\n\nChange-Id: {cid}"),
            ],
        );
        change_ids.push(cid);
    }
    (workdir, change_ids)
}

/// Mock GitHub for a push that creates `pr_numbers.len()` brand-new
/// PRs: an empty search (nothing on the remote yet), a POST that
/// hands out the numbers in order, and the comment endpoints the
/// stack comment needs.
async fn mock_github_creating(pr_numbers: &[u64]) -> MockServer {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(wm_path("/search/issues"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"items": []})))
        .mount(&server)
        .await;

    // One POST mock per PR, mounted newest-first so wiremock's
    // last-mounted-wins ordering hands out the numbers bottom-to-top
    // as the sequential upsert loop asks for them.
    for (i, number) in pr_numbers.iter().enumerate() {
        Mock::given(method("POST"))
            .and(wm_path("/repos/myorg/myrepo/pulls"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "number": number,
                "state": "open",
                "merged_at": null,
                "title": format!("Commit {}", (b'A' + u8::try_from(i).unwrap()) as char),
                "head": {"ref": format!("head-{number}"), "sha": "0".repeat(40)},
                "base": {"ref": "main"},
                "html_url": format!("https://github.com/myorg/myrepo/pull/{number}"),
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
    }

    // Stack comments: none exist, so each PR gets a POST.
    Mock::given(method("GET"))
        .and(path_regex(r"^/repos/myorg/myrepo/issues/\d+/comments$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(r"^/repos/myorg/myrepo/issues/\d+/comments$"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
        .mount(&server)
        .await;

    server
}

/// Mock GitHub for a push over two PRs that already exist **and are
/// already members of native stack #7**. Both PR payloads carry the
/// `stack` object exactly as GitHub puts it on the default API version.
///
/// `top_base` is the base branch GitHub reports for the top PR: pass
/// `bottom_ref` for a stack that is correctly chained (the routine
/// push, which must not touch the registration) or anything else for
/// one the planner has to retarget (the case that needs the fence).
///
/// The `/stacks` endpoints are deliberately *not* mounted here — every
/// test mounts the ones it expects, and asserts on the request log
/// rather than on mock absence: an unmounted `/stacks` call 404s, and
/// both `unstack` and `register` treat a 404 as a non-event by design.
async fn mock_github_updating_a_stacked_pair(
    bottom_ref: &str,
    top_ref: &str,
    top_base: &str,
    head_sha: &str,
) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wm_path("/search/issues"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [{"number": 101}, {"number": 102}],
        })))
        .mount(&server)
        .await;
    for (i, number) in [101_u64, 102].iter().enumerate() {
        let (head, base) = if i == 0 {
            (bottom_ref, "main")
        } else {
            (top_ref, top_base)
        };
        Mock::given(method("GET"))
            .and(wm_path(format!("/repos/myorg/myrepo/pulls/{number}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": number,
                "state": "open",
                "merged_at": null,
                "draft": false,
                "title": "existing",
                "body": "existing",
                "head": {"ref": head, "sha": head_sha},
                "base": {"ref": base},
                "html_url": format!("https://github.com/myorg/myrepo/pull/{number}"),
                "stack": {"id": 162_170, "number": 7, "position": i + 1, "size": 2},
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wm_path(format!(
                "/repos/myorg/myrepo/pulls/{number}/reviews"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(wm_path(format!("/repos/myorg/myrepo/pulls/{number}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path_regex(r"^/repos/myorg/myrepo/issues/\d+/comments$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(r"^/repos/myorg/myrepo/issues/\d+/comments$"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
        .mount(&server)
        .await;
    server
}

fn run_push(local: &Path, server_uri: &str, extra: &[&str]) -> std::process::Output {
    let mut args = vec![
        "stack",
        "push",
        "--trunk",
        "origin/main",
        "--author",
        "tester",
        "--branch-prefix",
        "stack/tester",
        // Bypass slug discovery — `origin` is a local tempdir path.
        "--repo",
        "myorg/myrepo",
        // Keep the test hermetic: no rebase round-trips, no
        // revision-history comments (Creates have no history anyway).
        "--skip-rebase",
        "--no-revision-history",
    ];
    args.extend_from_slice(extra);
    Command::new(mergify_binary())
        .args(&args)
        .current_dir(local)
        .env("MERGIFY_TOKEN", "test-token")
        .env("MERGIFY_GITHUB_SERVER", server_uri)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
}

fn assert_success(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "push failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// `METHOD /path` for each request the server saw, in order.
async fn request_log(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| format!("{} {}", r.method.as_str(), r.url.path()))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_the_flag_no_stacks_request_is_ever_sent() {
    // The load-bearing regression test for "default behaviour is
    // byte-identical": the feature is invisible unless asked for.
    let (work, _) = build_stack_repo(2);
    let local = work.path().join("local");
    let server = mock_github_creating(&[101, 102]).await;

    assert_success(&run_push(&local, &server.uri(), &[]));

    let log = request_log(&server).await;
    assert!(
        log.iter().all(|r| !r.contains("/stacks")),
        "flag off must not touch the Stacks API, got: {log:#?}",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_flag_registers_the_stack_after_every_pull_request_is_upserted() {
    let (work, _) = build_stack_repo(2);
    let local = work.path().join("local");
    let server = mock_github_creating(&[101, 102]).await;
    Mock::given(method("POST"))
        .and(wm_path("/repos/myorg/myrepo/stacks"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"number": 12})))
        .expect(1)
        .mount(&server)
        .await;

    let output = run_push(&local, &server.uri(), &["--github-native"]);
    assert_success(&output);

    let log = request_log(&server).await;
    let register = log
        .iter()
        .position(|r| r == "POST /repos/myorg/myrepo/stacks")
        .unwrap_or_else(|| panic!("no stack registration in {log:#?}"));
    let last_create = log
        .iter()
        .rposition(|r| r == "POST /repos/myorg/myrepo/pulls")
        .expect("PRs are created");
    assert!(
        register > last_create,
        "the stack must be registered only once every PR exists, got: {log:#?}",
    );

    // Members are sent bottom-to-top, as JSON integers.
    let body: serde_json::Value = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .find(|r| r.url.path() == "/repos/myorg/myrepo/stacks" && r.method.as_str() == "POST")
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .unwrap();
    assert_eq!(body, serde_json::json!({"pull_requests": [101, 102]}));

    // And the user is told, without it looking like a failure.
    let out = String::from_utf8_lossy(&output.stdout) + String::from_utf8_lossy(&output.stderr);
    assert!(out.contains("GitHub stack #12"), "output was: {out}");
}

/// Stack-branch name `mergify stack push` derives for commit `i` of a
/// repo built by [`build_stack_repo`].
fn head_ref(change_ids: &[String], i: usize) -> String {
    format!(
        "stack/tester/feature/commit-{}--{}",
        (b'a' + u8::try_from(i).unwrap()) as char,
        &change_ids[i][1..9],
    )
}

/// A repo of `n_commits` whose first `n_existing` stack branches are
/// already on the remote, parked on trunk.
///
/// Those commits then read as Updates of pull requests that already
/// exist — the branches must really be there at the SHA the mocked
/// payloads claim, or the push's force-with-lease rejects them, and
/// parking them on trunk (rather than at the local commit) is what
/// keeps them from planning as up-to-date and issuing no PATCH at all.
fn repo_with_pushed_branches(
    n_commits: usize,
    n_existing: usize,
) -> (tempfile::TempDir, PathBuf, Vec<String>, String) {
    let (work, change_ids) = build_stack_repo(n_commits);
    let local = work.path().join("local");
    let remote_head = capture(&local, &["rev-parse", "origin/main"]);
    for i in 0..n_existing {
        run_in(
            &local,
            &[
                "push",
                "-q",
                "origin",
                &format!("{remote_head}:refs/heads/{}", head_ref(&change_ids, i)),
            ],
        );
    }
    (work, local, change_ids, remote_head)
}

/// Bodies of every `PATCH /repos/myorg/myrepo/pulls/{n}` the server
/// saw, in order.
async fn pull_patch_bodies(server: &MockServer) -> Vec<serde_json::Value> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| {
            r.method.as_str() == "PATCH" && r.url.path().starts_with("/repos/myorg/myrepo/pulls/")
        })
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_routine_push_leaves_the_registration_completely_alone() {
    // The answer to "does this churn the API on every push?": no. The
    // stack lock is only about `base`, so a push that just refreshes
    // commits sends no `base`, and therefore needs no unstack, no
    // re-registration — no `/stacks` request at all. The PRs keep
    // their stack number and watchers see no new `pull_request.stacked`
    // events.
    let (_work, local, change_ids, remote_head) = repo_with_pushed_branches(2, 2);
    let server = mock_github_updating_a_stacked_pair(
        &head_ref(&change_ids, 0),
        &head_ref(&change_ids, 1),
        // Already correctly chained: nothing is being retargeted.
        &head_ref(&change_ids, 0),
        &remote_head,
    )
    .await;

    let output = run_push(&local, &server.uri(), &["--github-native"]);
    assert_success(&output);

    let log = request_log(&server).await;
    assert!(
        log.iter().all(|r| !r.contains("/stacks")),
        "a push that only refreshes commits must not touch the \
         registration, got: {log:#?}",
    );
    // What makes that safe: no `base` key in the update bodies.
    let bodies = pull_patch_bodies(&server).await;
    assert_eq!(bodies.len(), 2, "both PRs are updated: {bodies:#?}");
    for body in &bodies {
        assert!(
            body.get("base").is_none(),
            "an unchanged base must not be sent — GitHub 422s the whole \
             PATCH while the PR is stacked, got: {body}",
        );
    }
    let out = String::from_utf8_lossy(&output.stdout) + String::from_utf8_lossy(&output.stderr);
    assert!(out.contains("GitHub stack #7 unchanged"), "output: {out}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_change_pushed_on_top_extends_the_stack_instead_of_rebuilding_it() {
    // Appending is the other common stack operation, and GitHub has an
    // endpoint for it. One `POST /stacks/7/add` keeps stack #7 — its
    // number, its webhooks, its members' registration — where a
    // dissolve + re-register would replace all of it.
    let (_work, local, change_ids, remote_head) = repo_with_pushed_branches(3, 2);
    let server = mock_github_updating_a_stacked_pair(
        &head_ref(&change_ids, 0),
        &head_ref(&change_ids, 1),
        &head_ref(&change_ids, 0),
        &remote_head,
    )
    .await;
    // The third commit has no PR yet.
    Mock::given(method("POST"))
        .and(wm_path("/repos/myorg/myrepo/pulls"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "number": 103,
            "state": "open",
            "merged_at": null,
            "title": "Commit C",
            "head": {"ref": head_ref(&change_ids, 2), "sha": "0".repeat(40)},
            "base": {"ref": head_ref(&change_ids, 1)},
            "html_url": "https://github.com/myorg/myrepo/pull/103",
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(wm_path("/repos/myorg/myrepo/stacks/7/add"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"number": 7})))
        .expect(1)
        .mount(&server)
        .await;

    let output = run_push(&local, &server.uri(), &["--github-native"]);
    assert_success(&output);

    let log = request_log(&server).await;
    assert!(
        !log.iter().any(|r| r.ends_with("/stacks/7/unstack")),
        "extending must not dissolve the stack, got: {log:#?}",
    );
    assert!(
        !log.iter().any(|r| r == "POST /repos/myorg/myrepo/stacks"),
        "extending must not re-register the stack, got: {log:#?}",
    );
    // Only the new PR is appended, and only once every PR exists.
    let add = log
        .iter()
        .position(|r| r == "POST /repos/myorg/myrepo/stacks/7/add")
        .unwrap_or_else(|| panic!("no append in {log:#?}"));
    let last_create = log
        .iter()
        .rposition(|r| r == "POST /repos/myorg/myrepo/pulls")
        .expect("the new PR is created");
    assert!(add > last_create, "append after the create, got: {log:#?}");
    let body: serde_json::Value = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .find(|r| r.url.path() == "/repos/myorg/myrepo/stacks/7/add")
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .unwrap();
    assert_eq!(body, serde_json::json!({"pull_requests": [103]}));
    let out = String::from_utf8_lossy(&output.stdout) + String::from_utf8_lossy(&output.stderr);
    assert!(out.contains("added to GitHub stack #7"), "output: {out}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stack_that_cannot_be_extended_is_rebuilt() {
    // The append is best effort: a 422 (the new PR doesn't chain onto
    // the current top) or a 404 (someone dissolved the stack in the
    // meantime) must leave a registration that describes reality, not
    // a stale one. Safe to repair here because the mutations are done.
    let (_work, local, change_ids, remote_head) = repo_with_pushed_branches(3, 2);
    let server = mock_github_updating_a_stacked_pair(
        &head_ref(&change_ids, 0),
        &head_ref(&change_ids, 1),
        &head_ref(&change_ids, 0),
        &remote_head,
    )
    .await;
    Mock::given(method("POST"))
        .and(wm_path("/repos/myorg/myrepo/pulls"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "number": 103,
            "state": "open",
            "merged_at": null,
            "title": "Commit C",
            "head": {"ref": head_ref(&change_ids, 2), "sha": "0".repeat(40)},
            "base": {"ref": head_ref(&change_ids, 1)},
            "html_url": "https://github.com/myorg/myrepo/pull/103",
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(wm_path("/repos/myorg/myrepo/stacks/7/add"))
        .respond_with(ResponseTemplate::new(422).set_body_string("nope"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(wm_path("/repos/myorg/myrepo/stacks/7/unstack"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(wm_path("/repos/myorg/myrepo/stacks"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"number": 14})))
        .expect(1)
        .mount(&server)
        .await;

    let output = run_push(&local, &server.uri(), &["--github-native"]);
    assert_success(&output);

    let body: serde_json::Value = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .find(|r| r.url.path() == "/repos/myorg/myrepo/stacks" && r.method.as_str() == "POST")
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .unwrap();
    assert_eq!(body, serde_json::json!({"pull_requests": [101, 102, 103]}));
    let out = String::from_utf8_lossy(&output.stdout) + String::from_utf8_lossy(&output.stderr);
    assert!(out.contains("GitHub stack #14"), "output: {out}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_registered_stack_is_dissolved_before_a_pull_request_is_retargeted() {
    // The fence. GitHub rejects `PATCH /pulls/{n}` carrying `base`
    // while the PR is stacked — and the failed retarget is what lets
    // the orphan teardown close a surviving PR for good. So when this
    // push does move a base, the unstack has to precede every
    // PR-mutating call, not just the `neutralize_stale_bases` one.
    let (_work, local, change_ids, remote_head) = repo_with_pushed_branches(2, 2);
    let server = mock_github_updating_a_stacked_pair(
        &head_ref(&change_ids, 0),
        &head_ref(&change_ids, 1),
        // Top PR sits on a branch that is no longer its predecessor —
        // the shape a reorder leaves behind. The push must retarget it.
        "stack/tester/feature/commit-z--Ideadbee",
        &remote_head,
    )
    .await;
    Mock::given(method("POST"))
        .and(wm_path("/repos/myorg/myrepo/stacks/7/unstack"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(wm_path("/repos/myorg/myrepo/stacks"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"number": 13})))
        .expect(1)
        .mount(&server)
        .await;

    assert_success(&run_push(&local, &server.uri(), &["--github-native"]));

    let log = request_log(&server).await;
    let unstack = log
        .iter()
        .position(|r| r == "POST /repos/myorg/myrepo/stacks/7/unstack")
        .unwrap_or_else(|| panic!("no unstack in {log:#?}"));
    let first_mutation = log
        .iter()
        .position(|r| r.starts_with("PATCH /repos/myorg/myrepo/pulls/"))
        .unwrap_or_else(|| panic!("no PR update in {log:#?}"));
    let register = log
        .iter()
        .position(|r| r == "POST /repos/myorg/myrepo/stacks")
        .unwrap_or_else(|| panic!("no re-registration in {log:#?}"));
    assert!(
        unstack < first_mutation,
        "unstack must precede every PR mutation, got: {log:#?}",
    );
    assert!(
        register > first_mutation,
        "re-registration must follow the mutations, got: {log:#?}",
    );
    // And the retarget really is sent — otherwise this test would pass
    // for the wrong reason.
    let bodies = pull_patch_bodies(&server).await;
    assert!(
        bodies.iter().any(|b| b.get("base").is_some()),
        "the moving PR must carry `base`, got: {bodies:#?}",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_repository_without_the_stacks_api_still_pushes_cleanly() {
    // Old GHES, or a repo where the feature isn't enabled: the PRs
    // are all correct, so a 404 on registration must not colour the
    // exit code or read as an error.
    let (work, _) = build_stack_repo(2);
    let local = work.path().join("local");
    let server = mock_github_creating(&[101, 102]).await;
    Mock::given(method("POST"))
        .and(wm_path("/repos/myorg/myrepo/stacks"))
        .respond_with(ResponseTemplate::new(404).set_body_string("Not Found"))
        .mount(&server)
        .await;

    let output = run_push(&local, &server.uri(), &["--github-native"]);
    assert_success(&output);
    let out = String::from_utf8_lossy(&output.stdout) + String::from_utf8_lossy(&output.stderr);
    assert!(
        out.contains("not registered on GitHub"),
        "the skip should be stated plainly, got: {out}",
    );
    assert!(
        !out.contains("mergify: "),
        "degrading must not print an error, got: {out}",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_one_change_stack_stays_a_plain_pull_request() {
    // GitHub rejects a 1-PR stack with 422, so we don't even ask.
    let (work, _) = build_stack_repo(1);
    let local = work.path().join("local");
    let server = mock_github_creating(&[101]).await;

    assert_success(&run_push(&local, &server.uri(), &["--github-native"]));

    let log = request_log(&server).await;
    assert!(
        log.iter().all(|r| !r.contains("/stacks")),
        "below the 2-PR floor nothing should be sent, got: {log:#?}",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_config_can_turn_the_feature_on_without_the_flag() {
    let (work, _) = build_stack_repo(2);
    let local = work.path().join("local");
    run_in(
        &local,
        &["config", "mergify-cli.stack-github-native", "true"],
    );
    let server = mock_github_creating(&[101, 102]).await;
    Mock::given(method("POST"))
        .and(wm_path("/repos/myorg/myrepo/stacks"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"number": 12})))
        .expect(1)
        .mount(&server)
        .await;

    assert_success(&run_push(&local, &server.uri(), &[]));
}
