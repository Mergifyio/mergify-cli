//! End-to-end tests for `mergify stack sync`. Spawns the real
//! binary against a wiremock GitHub server and a real git repo
//! so the dry-run / drop-merged / pull-rebase paths are all
//! exercised.

use std::path::{Path, PathBuf};
use std::process::Command;

fn mergify_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mergify"))
}

fn isolated_git() -> Command {
    let mut cmd = Command::new("git");
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
    cmd.env("GIT_CONFIG_NOSYSTEM", "1");
    cmd
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

fn capture(dir: &Path, args: &[&str]) -> String {
    let out = isolated_git()
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// `feature` branch with `n_commits` commits, each carrying its
/// own Change-Id; the first commit on `feature` is the merge base
/// with `main`.
fn build_stack_repo(n_commits: usize) -> (tempfile::TempDir, Vec<(String, String)>) {
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

    let mut commits = Vec::new();
    for i in 0..n_commits {
        let label = (b'A' + u8::try_from(i).expect("test stack stays under 26 commits")) as char;
        let fname = format!("{}.txt", label.to_lowercase());
        std::fs::write(local.join(&fname), format!("content {label}")).unwrap();
        run_in(&local, &["add", &fname]);
        // Use the index in hex so each commit has a distinct
        // Change-Id while still being deterministic.
        let cid = format!("I{:040x}", i + 1);
        let msg = format!("Commit {label}\n\nChange-Id: {cid}");
        run_in(&local, &["commit", "-q", "-m", &msg]);
        commits.push((capture(&local, &["rev-parse", "HEAD"]), cid));
    }
    (workdir, commits)
}

async fn start_mock_with_no_prs() -> wiremock::MockServer {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    // Empty `/user` so the binary still reaches the empty-author
    // path quickly.
    Mock::given(method("GET"))
        .and(path("/user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "login": "tester",
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/search/issues"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"items": []})))
        .mount(&server)
        .await;
    server
}

fn run_mergify(local: &Path, server_uri: &str, args: &[&str]) -> std::process::Output {
    Command::new(mergify_binary())
        .args(args)
        .current_dir(local)
        .env("MERGIFY_TOKEN", "test-token")
        // Bypass git config + https coercion in
        // `stack_context::resolve_github_server` so we can point
        // at the wiremock URL verbatim (it speaks http, not https).
        .env("MERGIFY_GITHUB_SERVER", server_uri)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_dry_run_reports_up_to_date_when_no_merged_prs() {
    let (work, _) = build_stack_repo(2);
    let local = work.path().join("local");
    let server = start_mock_with_no_prs().await;

    let output = run_mergify(
        &local,
        &server.uri(),
        &[
            "stack",
            "sync",
            "--dry-run",
            "--trunk",
            "origin/main",
            "--author",
            "tester",
            "--branch-prefix",
            "stack/tester",
            // Bypass slug discovery (the test's `origin` URL is a
            // local tempdir path, not a parseable HTTPS/SSH URL).
            "--repo",
            "myorg/myrepo",
        ],
    );
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("up to date"),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_dry_run_lists_merged_commits() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let (work, commits) = build_stack_repo(2);
    let local = work.path().join("local");
    let first_sha = commits[0].0.clone();
    let first_cid = commits[0].1.clone();

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search/issues"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [{"number": 42}],
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/myorg/myrepo/pulls/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "number": 42,
            "title": "Commit A",
            "state": "closed",
            "merged_at": "2025-01-01T00:00:00Z",
            "head": {"sha": first_sha, "ref": format!("stack/tester/feature/feat-a--{}", &first_cid[1..9])},
            "base": {"ref": "main"},
            "html_url": "https://github.com/myorg/myrepo/pull/42",
        })))
        .mount(&server)
        .await;

    let output = run_mergify(
        &local,
        &server.uri(),
        &[
            "stack",
            "sync",
            "--dry-run",
            "--trunk",
            "origin/main",
            "--author",
            "tester",
            "--branch-prefix",
            "stack/tester",
            "--repo",
            "myorg/myrepo",
        ],
    );
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Dry run"), "stdout: {stdout}");
    assert!(stdout.contains("#42"), "stdout: {stdout}");
}

/// Real (non-dry-run) sync where the merged commit was
/// squash-merged into trunk with an identical patch. Git's default
/// `--no-reapply-cherry-picks` would silently omit it from the
/// rebase todo, leaving the drop editor with no `pick` line to
/// remove — the abort we guard against with `--reapply-cherry-picks`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_drops_squash_merged_commit_with_matching_patch() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let (work, commits) = build_stack_repo(2);
    let local = work.path().join("local");
    let a_sha = commits[0].0.clone();
    let a_cid = commits[0].1.clone();

    // Simulate the merge-queue squashing commit A onto trunk: a new
    // commit on `main` that adds the *same* `a.txt` with the *same*
    // content, so its patch-id matches commit A and git treats A as
    // already-applied during the rebase.
    run_in(&local, &["checkout", "-q", "main"]);
    std::fs::write(local.join("a.txt"), "content A").unwrap();
    run_in(&local, &["add", "a.txt"]);
    run_in(&local, &["commit", "-q", "-m", "Commit A (squashed #42)"]);
    run_in(&local, &["push", "-q", "origin", "main"]);
    run_in(&local, &["checkout", "-q", "feature"]);

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search/issues"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [{"number": 42}],
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/myorg/myrepo/pulls/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "number": 42,
            "title": "Commit A",
            "state": "closed",
            "merged_at": "2025-01-01T00:00:00Z",
            "head": {"sha": a_sha, "ref": format!("stack/tester/feature/feat-a--{}", &a_cid[1..9])},
            "base": {"ref": "main"},
            "html_url": "https://github.com/myorg/myrepo/pull/42",
        })))
        .mount(&server)
        .await;

    let output = run_mergify(
        &local,
        &server.uri(),
        &[
            "stack",
            "sync",
            "--trunk",
            "origin/main",
            "--author",
            "tester",
            "--branch-prefix",
            "stack/tester",
            "--repo",
            "myorg/myrepo",
        ],
    );
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // Commit A is gone; only commit B remains, rebased onto the new
    // trunk tip (which now carries the squashed a.txt).
    let ahead = capture(&local, &["rev-list", "--count", "origin/main..HEAD"]);
    assert_eq!(ahead, "1", "exactly commit B should remain on the stack");
    let a_reachable = isolated_git()
        .arg("-C")
        .arg(&local)
        .args(["merge-base", "--is-ancestor", &a_sha, "HEAD"])
        .status()
        .unwrap()
        .success();
    assert!(!a_reachable, "the squash-merged commit A should be dropped");
    let head_subject = capture(&local, &["log", "-1", "--format=%s"]);
    assert_eq!(head_subject, "Commit B");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_rejects_auto_generated_branch_names() {
    // Create a repo whose current branch matches the
    // auto-generated `<prefix>/<name>/<slug>--<hex8>` shape.
    let work = tempfile::tempdir().unwrap();
    let local = work.path().join("local");
    std::fs::create_dir(&local).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "t@e.com"],
        &["config", "user.name", "T"],
        &["commit", "--allow-empty", "-m", "root"],
        &[
            "checkout",
            "-q",
            "-b",
            "stack/tester/feature/feat-a--abcd1234",
        ],
    ] {
        isolated_git()
            .arg("-C")
            .arg(&local)
            .args(args)
            .status()
            .unwrap();
    }

    let server = start_mock_with_no_prs().await;
    let output = run_mergify(
        &local,
        &server.uri(),
        &[
            "stack",
            "sync",
            "--dry-run",
            "--trunk",
            "origin/main",
            "--author",
            "tester",
            "--branch-prefix",
            "stack/tester",
            "--repo",
            "myorg/myrepo",
        ],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("generated"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
}
