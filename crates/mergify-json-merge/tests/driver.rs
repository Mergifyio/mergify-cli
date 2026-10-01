//! `run`: the file handling around the merge, and the line-merge
//! fallback a decline hands over to.

use std::path::PathBuf;

use mergify_core::CliError;
use mergify_json_merge::DriverOptions;
use mergify_json_merge::run;

struct Files {
    _dir: tempfile::TempDir,
    ours: PathBuf,
    base: PathBuf,
    theirs: PathBuf,
}

fn files(base: &str, ours: &str, theirs: &str) -> Files {
    let dir = tempfile::tempdir().unwrap();
    let path = |name: &str, content: &str| {
        let p = dir.path().join(name);
        std::fs::write(&p, content).unwrap();
        p
    };
    Files {
        ours: path("ours", ours),
        base: path("base", base),
        theirs: path("theirs", theirs),
        _dir: dir,
    }
}

fn merge(f: &Files) -> (Result<(), CliError>, String) {
    let result = run(&DriverOptions {
        ours: &f.ours,
        base: &f.base,
        theirs: &f.theirs,
        marker_size: None,
        path: Some("x.json"),
    });
    (result, std::fs::read_to_string(&f.ours).unwrap())
}

#[test]
fn a_structural_merge_is_written_over_ours() {
    let f = files("[1, 2, 3]\n", "[0, 2, 3]\n", "[1, 2, 4]\n");
    let (result, ours) = merge(&f);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(ours, "[0, 2, 4]\n");
}

#[test]
fn the_trivial_cases_need_no_parse() {
    // Not JSON at all, and still settled: one side unchanged.
    let f = files("a\n", "b\n", "a\n");
    assert!(merge(&f).0.is_ok());
    assert_eq!(merge(&f).1, "b\n");
    let f = files("a\n", "a\n", "c\n");
    assert_eq!(merge(&f).1, "c\n");
}

#[test]
fn a_decline_leaves_git_conflict_markers() {
    let f = files("{\"a\": 1}\n", "{\"a\": 2}\n", "{\"a\": 3}\n");
    let (result, ours) = merge(&f);
    let Err(CliError::Conflict(msg)) = result else {
        panic!("{result:?}")
    };
    assert_eq!(
        msg,
        "x.json: both sides changed this value differently (at `/a`); left conflict markers"
    );
    assert_eq!(
        ours,
        "<<<<<<< ours\n{\"a\": 2}\n=======\n{\"a\": 3}\n>>>>>>> theirs\n"
    );
}

#[test]
fn not_json_falls_back_to_the_line_merge() {
    // A comment makes it JSONC; git's line merge is what it gets, as it
    // would without the driver.
    let base = "// x\n{\n  \"a\": 1,\n  \"b\": 2,\n  \"c\": 3\n}\n";
    let f = files(
        base,
        &base.replace("\"a\": 1", "\"a\": 0"),
        &base.replace("\"c\": 3", "\"c\": 4"),
    );
    let (result, ours) = merge(&f);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        ours,
        base.replace("\"a\": 1", "\"a\": 0")
            .replace("\"c\": 3", "\"c\": 4")
    );
}

#[test]
fn a_clean_line_merge_into_invalid_json_is_a_conflict() {
    // Both sides add `n`, with different values, at opposite ends of
    // the object: a structural decline, and lines far enough apart for
    // the line merge to call it clean — with `n` in it twice.
    let base = "{\n  \"a\": 1,\n  \"b\": 2,\n  \"c\": 3\n}\n";
    let ours = base.replace("{\n", "{\n  \"n\": 1,\n");
    let theirs = base.replace("\"c\": 3\n", "\"c\": 3,\n  \"n\": 2\n");
    let f = files(base, &ours, &theirs);
    let (result, merged) = merge(&f);
    assert!(
        merged.contains("\"n\": 1") && merged.contains("\"n\": 2"),
        "{merged}"
    );
    let Err(CliError::Conflict(msg)) = result else {
        panic!("{result:?}")
    };
    assert_eq!(
        msg,
        "x.json: both sides added this value, differently (at `/n`); \
         the line merge git falls back to produced invalid JSON"
    );
}
