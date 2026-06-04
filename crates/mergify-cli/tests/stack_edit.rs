//! End-to-end tests for `mergify stack edit`.
//!
//! These need the freshly-built `mergify` binary on disk to drive
//! `git rebase -i` through its `GIT_SEQUENCE_EDITOR`
//! self-invocation, so they live next to the binary crate where
//! `CARGO_BIN_EXE_mergify` is set by `cargo test`. The pure
//! pieces (commit prefix matching, rebase-todo rewriting) are
//! covered by unit tests in
//! `crates/mergify-stack/src/{commands/edit.rs,rebase_todo.rs}`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// `CARGO_BIN_EXE_mergify` is set by `cargo test` for tests in
/// the crate that produces the binary. We assert it's present
/// because the rebase machinery only makes sense against the
/// real binary; falling back to PATH would pick up whichever
/// `mergify` is installed system-wide.
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

/// Bare upstream + local clone with three commits on `feature`,
/// each carrying a Change-Id trailer (the local-commits walker
/// requires it). Returned tuple: `(workdir, [(sha, change_id)])`.
fn build_stack_repo() -> (tempfile::TempDir, Vec<(String, String)>) {
    let workdir = tempfile::tempdir().unwrap();
    let upstream = workdir.path().join("up.git");
    let ok = Command::new("git")
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

fn run_mergify_edit(local: &Path, target: &str) -> std::process::Output {
    Command::new(mergify_binary())
        .args(["stack", "edit", target])
        .current_dir(local)
        .output()
        .unwrap()
}

#[test]
fn stack_edit_pauses_at_target_commit() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");

    let output = run_mergify_edit(&local, &commits[1].0[..12]);
    assert!(
        output.status.success(),
        "mergify stack edit failed: {:?}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Editing commit:"));
    assert_eq!(capture(&local, &["log", "-1", "--format=%s"]), "Commit B");
    assert!(local.join(".git/rebase-merge").exists());

    Command::new("git")
        .arg("-C")
        .arg(&local)
        .args(["rebase", "--abort"])
        .status()
        .unwrap();
}

#[test]
fn stack_edit_pauses_by_change_id_prefix() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");

    let output = run_mergify_edit(&local, &commits[1].1[..9]);
    assert!(
        output.status.success(),
        "mergify stack edit failed: {:?}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(capture(&local, &["log", "-1", "--format=%s"]), "Commit B");

    Command::new("git")
        .arg("-C")
        .arg(&local)
        .args(["rebase", "--abort"])
        .status()
        .unwrap();
}

#[test]
fn stack_edit_unknown_prefix_exits_nonzero() {
    let (work, _) = build_stack_repo();
    let local = work.path().join("local");

    let output = run_mergify_edit(&local, "deadbeef1234");
    assert!(!output.status.success(), "should have failed");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("deadbeef1234"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn stack_edit_empty_stack_prints_message() {
    // Local clone where `feature` is at the same commit as `main`
    // — no commits between trunk and HEAD.
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
        &["commit", "--allow-empty", "-m", "root"],
        &["remote", "add", "origin", upstream.to_str().unwrap()],
        &["push", "-q", "origin", "main"],
        &["remote", "set-head", "origin", "main"],
        &["checkout", "-q", "-b", "feature"],
    ] {
        run_in(&local, args);
    }

    let output = run_mergify_edit(&local, "anything");
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("No commits in the stack"),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout),
    );
}
