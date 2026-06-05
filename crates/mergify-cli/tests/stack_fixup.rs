//! End-to-end tests for `mergify stack fixup`. Spawns the
//! freshly-built binary so the rebase-todo-rewrite self-invocation
//! is exercised end to end.

use std::path::{Path, PathBuf};
use std::process::Command;

fn mergify_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mergify"))
}

fn run_in(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap_or_else(|e| panic!("spawn git -C {}: {args:?}: {e}", dir.display()))
        .success();
    assert!(ok, "git -C {}: {args:?} failed", dir.display());
}

fn capture(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// `feature` branch on top of `origin/main` with three real
/// commits — non-empty so git can actually fold them.
fn build_stack_repo() -> (tempfile::TempDir, Vec<(String, String)>) {
    let workdir = tempfile::tempdir().unwrap();
    let upstream = workdir.path().join("up.git");
    Command::new("git")
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
    for (label, cid) in [
        ("A", "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01"),
        ("B", "Ibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb02"),
        ("C", "Icccccccccccccccccccccccccccccccccccccc03"),
    ] {
        let fname = format!("{}.txt", label.to_lowercase());
        std::fs::write(local.join(&fname), format!("content {label}")).unwrap();
        run_in(&local, &["add", &fname]);
        let msg = format!("Commit {label}\n\nChange-Id: {cid}");
        run_in(&local, &["commit", "-q", "-m", &msg]);
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

fn count_log_lines(dir: &Path) -> usize {
    capture(dir, &["log", "--format=%s", "origin/main..HEAD"])
        .lines()
        .count()
}

#[test]
fn fixup_folds_commit_into_parent() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");

    let output = run_mergify(&local, &["stack", "fixup", &commits[1].0[..12]]);
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // B folded into A: stack now contains [A, C] (HEAD is C).
    let subjects = capture(&local, &["log", "--format=%s", "origin/main..HEAD"]);
    let lines: Vec<&str> = subjects.lines().collect();
    assert_eq!(lines, ["Commit C", "Commit A"]);
}

#[test]
fn fixup_dry_run_does_not_modify_history() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");
    let head_before = capture(&local, &["rev-parse", "HEAD"]);

    let output = run_mergify(
        &local,
        &["stack", "fixup", "--dry-run", &commits[1].0[..12]],
    );
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Fixup plan:"));
    assert_eq!(capture(&local, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(count_log_lines(&local), 3);
}

#[test]
fn fixup_rejects_first_commit() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");

    let output = run_mergify(&local, &["stack", "fixup", &commits[0].0[..12]]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("first commit"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn fixup_unknown_prefix_exits_nonzero() {
    let (work, _) = build_stack_repo();
    let local = work.path().join("local");
    let output = run_mergify(&local, &["stack", "fixup", "deadbeef1234"]);
    assert!(!output.status.success());
}
