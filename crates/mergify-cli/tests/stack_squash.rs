//! End-to-end tests for `mergify stack squash`.

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
fn squash_src_into_target_keeps_target_message() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");

    // squash C into A (no -m). Expected: A holds A+C; B unchanged.
    let output = run_mergify(
        &local,
        &[
            "stack",
            "squash",
            &commits[2][..12], // C
            "into",
            &commits[0][..12], // A
        ],
    );
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let subjects = feature_subjects(&local);
    // After squash: [A (now containing A+C), B]
    assert_eq!(subjects, ["Commit A", "Commit B"]);
    // C's content is preserved (folded into A)
    assert!(local.join("a.txt").exists());
    assert!(local.join("c.txt").exists());
}

#[test]
fn squash_with_custom_message_replaces_subject() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");

    let output = run_mergify(
        &local,
        &[
            "stack",
            "squash",
            &commits[2][..12],
            "into",
            &commits[0][..12],
            "-m",
            "feat: combined A+C\n\nbody paragraph",
        ],
    );
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let subjects = feature_subjects(&local);
    assert!(subjects.contains(&"feat: combined A+C".to_string()));
    assert!(!subjects.contains(&"Commit A".to_string()));
    // Body survived the rebase-todo / tempfile indirection.
    let head_parent_body = capture(&local, &["log", "-1", "--format=%b", "HEAD~1"]);
    assert!(head_parent_body.contains("body paragraph"));
}

#[test]
fn squash_dry_run_does_not_modify_head() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");
    let head_before = capture(&local, &["rev-parse", "HEAD"]);

    let output = run_mergify(
        &local,
        &[
            "stack",
            "squash",
            "--dry-run",
            &commits[2][..12],
            "into",
            &commits[0][..12],
        ],
    );
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Squash plan:"));
    assert_eq!(capture(&local, &["rev-parse", "HEAD"]), head_before);
}

#[test]
fn squash_src_equals_target_errors() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");
    let output = run_mergify(
        &local,
        &[
            "stack",
            "squash",
            &commits[0][..12],
            "into",
            &commits[0][..12],
        ],
    );
    assert!(!output.status.success());
}

#[test]
fn squash_missing_into_keyword_errors() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");
    let output = run_mergify(
        &local,
        &["stack", "squash", &commits[0][..12], &commits[1][..12]],
    );
    assert!(!output.status.success());
}

#[test]
fn squash_multiple_srcs_into_target() {
    let (work, commits) = build_stack_repo();
    let local = work.path().join("local");

    let output = run_mergify(
        &local,
        &[
            "stack",
            "squash",
            &commits[1][..12], // B
            &commits[2][..12], // C
            "into",
            &commits[0][..12], // A
        ],
    );
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let subjects = feature_subjects(&local);
    // After squashing B+C into A: stack is just [A]
    assert_eq!(subjects, ["Commit A"]);
    assert!(local.join("a.txt").exists());
    assert!(local.join("b.txt").exists());
    assert!(local.join("c.txt").exists());
}
