//! End-to-end tests for `mergify stack list`. Spawns the real binary
//! against a wiremock GitHub server and a real git repo.

use std::path::{Path, PathBuf};
use std::process::Command;

use wiremock::matchers::{method, path, query_param};
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

/// `feature` branch with `n_commits` commits on top of a pushed
/// `main`, each carrying a Change-Id distinct in its first 8 hex —
/// the part a stack branch name carries.
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
        let cid = format!("I{:08x}{}", i + 1, "0".repeat(32));
        let msg = format!("Commit {label}\n\nChange-Id: {cid}");
        run_in(&local, &["commit", "-q", "-m", &msg]);
        commits.push((capture(&local, &["rev-parse", "HEAD"]), cid));
    }
    (workdir, commits)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_reads_mergeable_from_the_pull_request_itself() {
    // Stack discovery goes through the pull-request *list* endpoint,
    // which never carries `mergeable`. `stack list` has to ask the PR
    // for it, or it could no longer flag a conflicting one.
    let (work, commits) = build_stack_repo(2);
    let local = work.path().join("local");
    let head_ref = format!("stack/tester/feature/commit--{}", &commits[0].1[1..9]);

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/repos/myorg/myrepo/git/matching-refs/heads/stack/tester/feature/",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!([{"ref": format!("refs/heads/{head_ref}")}])),
        )
        .expect(1)
        .mount(&server)
        .await;
    let listed = serde_json::json!({
        "number": 101,
        "state": "open",
        "draft": false,
        "merged_at": null,
        "title": "Commit A",
        "user": {"login": "tester"},
        "updated_at": "2026-01-01T00:00:00Z",
        "head": {"ref": head_ref, "sha": commits[0].0},
        "base": {"ref": "main"},
        "html_url": "https://github.com/myorg/myrepo/pull/101",
    });
    Mock::given(method("GET"))
        .and(path("/repos/myorg/myrepo/pulls"))
        .and(query_param("head", format!("myorg:{head_ref}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([listed])))
        .expect(1)
        .mount(&server)
        .await;
    // Commit B has no PR yet: its branch is looked up and holds none.
    Mock::given(method("GET"))
        .and(path("/repos/myorg/myrepo/pulls"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .with_priority(10)
        .expect(1)
        .mount(&server)
        .await;
    let mut full = listed.clone();
    full["mergeable"] = serde_json::json!(false);
    Mock::given(method("GET"))
        .and(path("/repos/myorg/myrepo/pulls/101"))
        .respond_with(ResponseTemplate::new(200).set_body_json(full))
        .expect(1)
        .mount(&server)
        .await;
    mount_no_checks_or_reviews(&server, &commits[0].0, 101).await;

    let output = run_list(&local, &server.uri());
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let entries: Vec<_> = payload["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["pull_number"].clone(),
                e["status"].clone(),
                e["mergeable"].clone(),
            )
        })
        .collect();
    assert_eq!(
        entries,
        vec![
            (
                serde_json::json!(101),
                serde_json::json!("open"),
                serde_json::json!(false)
            ),
            (
                serde_json::Value::Null,
                serde_json::json!("no_pr"),
                serde_json::Value::Null
            ),
        ],
    );
}

fn run_list(local: &Path, server_uri: &str) -> std::process::Output {
    Command::new(mergify_binary())
        .args([
            "stack",
            "list",
            "--json",
            "--trunk",
            "origin/main",
            "--author",
            "tester",
            "--branch-prefix",
            "stack/tester",
            // Bypass slug discovery — `origin` is a local tempdir path.
            "--repository",
            "myorg/myrepo",
        ])
        .current_dir(local)
        .env("MERGIFY_TOKEN", "test-token")
        .env("MERGIFY_GITHUB_SERVER", server_uri)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
}

/// The CI and review columns: nothing to report.
async fn mount_no_checks_or_reviews(server: &MockServer, head_sha: &str, number: u64) {
    Mock::given(method("GET"))
        .and(path(format!(
            "/repos/myorg/myrepo/commits/{head_sha}/check-runs"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"check_runs": []})),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/repos/myorg/myrepo/pulls/{number}/reviews")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(server)
        .await;
}
