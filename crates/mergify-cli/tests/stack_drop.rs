//! End-to-end tests for `mergify stack drop`. Spawns the
//! freshly-built binary against real git repos so the rebase
//! todo-rewrite self-invocation is exercised.

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
        .unwrap_or_else(|e| panic!("spawn git -C {}: {args:?}: {e}", dir.display()))
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

fn count_log_lines(dir: &Path) -> usize {
    capture(dir, &["log", "--format=%s", "origin/main..HEAD"])
        .lines()
        .count()
}

/// Three-commit stack `A → B → C` on a `feature` branch tracking
/// `origin/main`, every commit carrying a Change-Id trailer.
fn build_stack_repo() -> (tempfile::TempDir, Vec<(String, String)>) {
    let workdir = tempfile::tempdir().unwrap();
    let upstream = workdir.path().join("up.git");
    let ok = isolated_git()
        .args([
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            upstream.to_str().unwrap(),
        ])
        .status()
        .unwrap()
        .success();
    assert!(ok);
    let local = workdir.path().join("local");
    std::fs::create_dir(&local).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "t@e.com"],
        &["config", "user.name", "T"],
        &["commit", "--allow-empty", "-m", "root"],
        &["remote", "add", "origin", upstream.to_str().unwrap()],
        &["push", "-q", "origin", "main"],
        &["remote", "set-head", "origin", "main"],
        &["checkout", "-q", "-b", "feature"],
    ] {
        run_in(&local, args);
    }
    let mut commits = Vec::new();
    for (label, cid) in [
        ("A", "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01"),
        ("B", "Ibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb02"),
        ("C", "Icccccccccccccccccccccccccccccccccccccc03"),
    ] {
        let msg = format!("Commit {label}\n\nChange-Id: {cid}");
        run_in(&local, &["commit", "--allow-empty", "-m", &msg]);
        let sha = capture(&local, &["rev-parse", "HEAD"]);
        commits.push((sha, cid.to_string()));
    }
    (workdir, commits)
}

fn run_mergify(local: &Path, args: &[&str]) -> std::process::Output {
    Command::new(mergify_binary())
        .args(args)
        .current_dir(local)
        .output()
        .unwrap()
}

#[test]
fn drop_removes_middle_commit() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");

    assert_eq!(count_log_lines(&local), 3);

    let output = run_mergify(&local, &["stack", "drop", &commits[1].0[..12]]);
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let subjects = capture(&local, &["log", "--format=%s", "origin/main..HEAD"]);
    let lines: Vec<&str> = subjects.lines().collect();
    // git log walks newest → oldest; with B dropped we expect
    // [C, A].
    assert_eq!(lines, ["Commit C", "Commit A"]);
}

#[test]
fn drop_removes_multiple_commits() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");

    let output = run_mergify(
        &local,
        &["stack", "drop", &commits[0].0[..12], &commits[2].0[..12]],
    );
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    assert_eq!(count_log_lines(&local), 1);
    assert_eq!(capture(&local, &["log", "-1", "--format=%s"]), "Commit B");
}

#[test]
fn drop_by_change_id() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");

    let output = run_mergify(&local, &["stack", "drop", &commits[1].1[..9]]);
    assert!(output.status.success());

    let subjects = capture(&local, &["log", "--format=%s", "origin/main..HEAD"]);
    let lines: Vec<&str> = subjects.lines().collect();
    assert_eq!(lines, ["Commit C", "Commit A"]);
}

#[test]
fn drop_dry_run_does_not_modify_history() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");
    let head_before = capture(&local, &["rev-parse", "HEAD"]);

    let output = run_mergify(&local, &["stack", "drop", "--dry-run", &commits[1].0[..12]]);
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Drop plan:"),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout),
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Dry run"),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout),
    );

    assert_eq!(capture(&local, &["rev-parse", "HEAD"]), head_before);
}

#[test]
fn drop_unknown_prefix_exits_nonzero() {
    let (work, _) = build_stack_repo();
    let local = work.path().join("local");
    let output = run_mergify(&local, &["stack", "drop", "deadbeef1234"]);
    assert!(!output.status.success());
}

#[test]
fn drop_duplicate_prefix_is_rejected() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");
    let output = run_mergify(
        &local,
        &["stack", "drop", &commits[1].0[..7], &commits[1].0[..12]],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("duplicate"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
}
