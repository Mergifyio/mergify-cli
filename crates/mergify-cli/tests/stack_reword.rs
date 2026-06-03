//! End-to-end tests for `mergify stack reword`. The `-m` path is
//! exercised end to end; the interactive path (no `-m`) opens an
//! editor we don't have in test contexts so we cover only its
//! dry-run resolution.

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
        .unwrap()
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

fn build_stack_repo() -> (tempfile::TempDir, Vec<String>) {
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
    ] {
        let fname = format!("{}.txt", label.to_lowercase());
        std::fs::write(local.join(&fname), format!("content {label}")).unwrap();
        run_in(&local, &["add", &fname]);
        let msg = format!("Commit {label}\n\nChange-Id: {cid}");
        run_in(&local, &["commit", "-q", "-m", &msg]);
        commits.push(capture(&local, &["rev-parse", "HEAD"]));
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
fn reword_with_message_replaces_subject() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");

    let output = run_mergify(
        &local,
        &[
            "stack",
            "reword",
            &commits[1][..12],
            "-m",
            "feat: new subject\n\nbody line",
        ],
    );
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let subject = capture(&local, &["log", "-1", "--format=%s"]);
    assert_eq!(subject, "feat: new subject");
    let body = capture(&local, &["log", "-1", "--format=%b"]);
    assert!(body.contains("body line"), "got body: {body}");
}

#[test]
fn reword_dry_run_does_not_modify_head() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");
    let head_before = capture(&local, &["rev-parse", "HEAD"]);
    let output = run_mergify(
        &local,
        &["stack", "reword", "--dry-run", &commits[1][..12], "-m", "x"],
    );
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Reword plan:"));
    assert_eq!(capture(&local, &["rev-parse", "HEAD"]), head_before);
}

#[test]
fn reword_unknown_prefix_exits_nonzero() {
    let (work, _) = build_stack_repo();
    let local = work.path().join("local");
    let output = run_mergify(&local, &["stack", "reword", "deadbeef1234", "-m", "x"]);
    assert!(!output.status.success());
}
