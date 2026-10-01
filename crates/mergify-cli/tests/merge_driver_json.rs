//! End-to-end tests for `mergify merge-driver json`: the freshly built
//! binary, run by a real git as a configured merge driver — once from a
//! working tree (`git merge`, a laptop) and once from a bare repository
//! (`git merge-tree` with `info/attributes`, how Mergify's merge queue
//! composes a batch).

use std::path::Path;
use std::process::Command;
use std::process::Output;

const BASE: &str = r#"{
  "name": "dashboard",
  "devDependencies": {
    "@types/lodash": "4.17.24",
    "@types/luxon": "3.7.1",
    "typescript": "5.9.2"
  }
}
"#;

fn git(dir: &Path, args: &[&str]) -> Output {
    let driver = format!(
        "'{}' merge-driver json --marker-size %L --path %P %A %O %B",
        env!("CARGO_BIN_EXE_mergify")
    );
    Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=t@e.com", "-c", "user.name=T"])
        .args(["-c", &format!("merge.json.driver={driver}")])
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"))
}

#[track_caller]
fn ok(dir: &Path, args: &[&str]) -> String {
    let out = git(dir, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A repository whose `main` holds `BASE`, with `ours` and `theirs`
/// branches each committing their own version of it.
fn repo(ours: &str, theirs: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    ok(d, &["init", "-q", "-b", "main"]);
    std::fs::write(d.join(".gitattributes"), "*.json merge=json\n").unwrap();
    std::fs::write(d.join("package.json"), BASE).unwrap();
    ok(d, &["add", "."]);
    ok(d, &["commit", "-q", "-m", "base"]);
    for (branch, content) in [("ours", ours), ("theirs", theirs)] {
        ok(d, &["checkout", "-q", "-b", branch, "main"]);
        std::fs::write(d.join("package.json"), content).unwrap();
        ok(d, &["commit", "-q", "-am", branch]);
    }
    ok(d, &["checkout", "-q", "ours"]);
    dir
}

#[test]
fn neighbouring_bumps_merge_in_a_working_tree() {
    let dir = repo(
        &BASE.replace("4.17.24", "4.17.25"),
        &BASE.replace("3.7.1", "3.7.4"),
    );
    ok(dir.path(), &["merge", "-q", "--no-edit", "theirs"]);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("package.json")).unwrap(),
        BASE.replace("4.17.24", "4.17.25").replace("3.7.1", "3.7.4")
    );
}

#[test]
fn the_same_dependency_bumped_twice_conflicts_with_markers() {
    let dir = repo(
        &BASE.replace("3.7.1", "3.7.4"),
        &BASE.replace("3.7.1", "3.8.0"),
    );
    let out = git(dir.path(), &["merge", "--no-edit", "theirs"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(
            "mergify: package.json: both sides changed this value differently \
             (at `/devDependencies/@types~1luxon`); left conflict markers"
        ),
        "{stderr}"
    );
    let content = std::fs::read_to_string(dir.path().join("package.json")).unwrap();
    assert!(
        content.contains("<<<<<<< ours\n    \"@types/luxon\": \"3.7.4\",\n=======\n"),
        "{content}"
    );
}

#[test]
fn a_bare_merge_tree_resolves_through_info_attributes() {
    // The merge queue's shape: no working tree, so the attributes come
    // from `$GIT_DIR/info/attributes` and the merge from `merge-tree`.
    let work = repo(
        &BASE.replace("4.17.24", "4.17.25"),
        &BASE.replace("3.7.1", "3.7.4"),
    );
    let bare = tempfile::tempdir().unwrap();
    let src = work.path().to_str().unwrap();
    ok(bare.path(), &["clone", "-q", "--bare", src, "."]);
    std::fs::write(
        bare.path().join("info/attributes"),
        "package.json merge=json\n",
    )
    .unwrap();
    let tree = ok(
        bare.path(),
        &["merge-tree", "--write-tree", "ours", "theirs"],
    );
    let merged = ok(
        bare.path(),
        &["cat-file", "-p", &format!("{tree}:package.json")],
    );
    assert_eq!(
        format!("{merged}\n"),
        BASE.replace("4.17.24", "4.17.25").replace("3.7.1", "3.7.4")
    );

    // Without the attribute, the same merge is the conflict the driver
    // exists to remove.
    std::fs::write(bare.path().join("info/attributes"), "").unwrap();
    let out = git(
        bare.path(),
        &["merge-tree", "--write-tree", "ours", "theirs"],
    );
    assert_eq!(out.status.code(), Some(1));
}
