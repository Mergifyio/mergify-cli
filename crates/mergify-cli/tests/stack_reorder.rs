//! End-to-end tests for `mergify stack reorder` and
//! `mergify stack move` — both reduce to the same
//! `Action::Reorder` machinery, so they share a test binary.

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
    assert!(ok);
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

/// `feature` branch with three non-empty commits A, B, C
/// (touching different files so reorder is conflict-free).
fn build_stack_repo() -> (tempfile::TempDir, Vec<String>) {
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

fn feature_subjects(local: &Path) -> Vec<String> {
    capture(
        local,
        &["log", "--reverse", "--format=%s", "origin/main..HEAD"],
    )
    .lines()
    .map(str::to_string)
    .collect()
}

#[test]
fn reorder_to_explicit_sequence() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");

    let output = run_mergify(
        &local,
        &[
            "stack",
            "reorder",
            &commits[2][..12], // C
            &commits[0][..12], // A
            &commits[1][..12], // B
        ],
    );
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let order = feature_subjects(&local);
    assert_eq!(order, ["Commit C", "Commit A", "Commit B"]);
}

#[test]
fn reorder_already_in_order_is_noop() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");
    let head_before = capture(&local, &["rev-parse", "HEAD"]);

    let output = run_mergify(
        &local,
        &[
            "stack",
            "reorder",
            &commits[0][..12],
            &commits[1][..12],
            &commits[2][..12],
        ],
    );
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("already"));
    assert_eq!(capture(&local, &["rev-parse", "HEAD"]), head_before);
}

#[test]
fn reorder_count_mismatch_errors() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");
    let output = run_mergify(&local, &["stack", "reorder", &commits[0][..12]]);
    assert!(!output.status.success());
}

#[test]
fn move_commit_first_brings_it_to_the_top() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");

    let output = run_mergify(&local, &["stack", "move", &commits[2][..12], "first"]);
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    assert_eq!(
        feature_subjects(&local),
        ["Commit C", "Commit A", "Commit B"]
    );
}

#[test]
fn move_commit_before_target() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");

    let output = run_mergify(
        &local,
        &[
            "stack",
            "move",
            &commits[2][..12], // C
            "before",
            &commits[0][..12], // A
        ],
    );
    assert!(output.status.success());
    assert_eq!(
        feature_subjects(&local),
        ["Commit C", "Commit A", "Commit B"]
    );
}

#[test]
fn move_before_without_target_errors() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");
    let output = run_mergify(&local, &["stack", "move", &commits[2][..12], "before"]);
    assert!(!output.status.success());
}

#[test]
fn move_first_with_target_errors() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");
    let output = run_mergify(
        &local,
        &[
            "stack",
            "move",
            &commits[2][..12],
            "first",
            &commits[0][..12],
        ],
    );
    assert!(!output.status.success());
}
