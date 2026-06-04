//! Walk the local commits in `<base>..<head>` and emit one
//! structured record per commit.
//!
//! Used by every stack subcommand as the first step: read what's
//! locally in the stack, paired with each commit's `Change-Id:`
//! trailer (the stable identity that survives rewrites and pairs
//! a local commit with its remote branch + PR).
//!
//! The on-disk shape comes straight from
//! `git log --reverse --format=%H%x00%s%x00%b%x1e <range>`:
//! one record per commit, fields separated by `NUL` (`\x00`),
//! records by ASCII Record Separator (`\x1e`). Picking control
//! bytes avoids the quoting tax of `--format=` with shell-safe
//! delimiters and the parser is a fixed-shape split rather than
//! line-aware reading.

use std::path::{Path, PathBuf};
use std::process::Command;

use mergify_core::CliError;
use serde::Serialize;

use crate::change_id;
use crate::slug;

/// `refs/notes/mergify/stack` — the notes ref `mergify stack`
/// uses to attach amend-reason notes to commits. Read alongside
/// the commit body in the walker via `git log --notes=…` so the
/// revision-history rendering doesn't need a per-commit subprocess
/// round-trip.
pub const STACK_NOTES_REF: &str = "refs/notes/mergify/stack";

/// One commit in the local stack range, after parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalCommit {
    /// Full 40-hex commit SHA.
    pub commit_sha: String,
    /// Commit subject line (the `%s` field).
    pub title: String,
    /// Full commit message body — caller-facing because some
    /// stack commands need to surface it (e.g. the revision
    /// history rendering reads it back). Mirrors Python's
    /// `commit_infos` tuple shape.
    pub message: String,
    /// The trailing `Change-Id:` value extracted from `message`.
    /// Always present in a `LocalCommit` — a commit missing the
    /// trailer fails the walk early so partial results never
    /// reach the caller.
    pub change_id: String,
    /// Deterministic `<slug>--<hex8>` branch suffix derived from
    /// `title` + `change_id` (see [`crate::slug::slugify_title`]).
    /// Used by stack commands as the default head-branch name
    /// when a local commit has no remote PR paired with it yet —
    /// pre-computing it here keeps the slug algorithm in one place
    /// regardless of how many stack callers need it.
    pub slug: String,
    /// Amend-reason note attached to the commit on the
    /// `refs/notes/mergify/stack` ref, or an empty string when
    /// the commit has no note. The walker reads notes through
    /// `git log --notes=…` so this is a free piggyback on the
    /// existing per-stack-op subprocess; before this field, the
    /// Python orchestrator ran one extra `git notes show <sha>`
    /// per commit (N round-trips). Consumed by the revision
    /// history rendering on the PR comment.
    pub note: String,
}

/// Run `git log` in `repo_dir` and parse its output into one
/// [`LocalCommit`] per commit in `<base>..<head>`.
///
/// `base` is anything `git` accepts as a revision (typically a
/// merge-base SHA); `head` is the local stack branch name (or
/// any revision). The range is **exclusive** of `base` and
/// **inclusive** of `head`, matching git's `..` semantics.
///
/// Errors:
///
/// - [`CliError::Generic`] for git invocation failures (process
///   spawn errors, git exiting non-zero, unparseable output).
/// - [`CliError::InvalidState`] when a commit in the range has no
///   `Change-Id:` trailer. Mirrors Python's
///   `console_error(...); sys.exit(ExitCode.INVALID_STATE)` flow.
pub fn read(repo_dir: &Path, base: &str, head: &str) -> Result<Vec<LocalCommit>, CliError> {
    let raw = run_git_log(repo_dir, base, head)?;
    parse(&raw)
}

fn run_git_log(repo_dir: &Path, base: &str, head: &str) -> Result<String, CliError> {
    let range = format!("{base}..{head}");
    let notes_arg = format!("--notes={STACK_NOTES_REF}");
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args([
            "log",
            "--reverse",
            // `%H` SHA, `%x00` NUL, `%s` subject, `%x00` NUL,
            // `%b` body, `%x00` NUL, `%N` note, `%x1e` ASCII RS
            // terminator. `%N` is empty when the commit has no
            // note on the configured ref, so the field count is
            // stable across both cases.
            "--format=%H%x00%s%x00%b%x00%N%x1e",
            &notes_arg,
            &range,
        ])
        .output()
        .map_err(|e| CliError::Generic(format!("failed to spawn `git log`: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(CliError::Generic(format!(
            "`git log {range}` failed: {}",
            if stderr.is_empty() {
                "no stderr".to_string()
            } else {
                stderr
            },
        )));
    }

    String::from_utf8(output.stdout)
        .map_err(|e| CliError::Generic(format!("`git log` output is not UTF-8: {e}")))
}

/// Parse the raw `git log` payload (NUL/RS separated) into
/// [`LocalCommit`] records. Exposed separately from [`read`] so
/// the format-handling can be unit-tested without spawning git.
pub fn parse(raw: &str) -> Result<Vec<LocalCommit>, CliError> {
    let mut out = Vec::new();
    for record in raw.split('\x1e') {
        let stripped = record.trim();
        if stripped.is_empty() {
            continue;
        }
        let mut parts = stripped.splitn(4, '\x00');
        let commit_sha = parts
            .next()
            .ok_or_else(|| malformed_record(stripped))?
            .trim()
            .to_string();
        let title = parts
            .next()
            .ok_or_else(|| malformed_record(stripped))?
            .trim()
            .to_string();
        let message = parts
            .next()
            .ok_or_else(|| malformed_record(stripped))?
            .trim()
            .to_string();
        // `%N` is empty when no note exists on the configured ref.
        // The field is still emitted by git so this never returns
        // `None`; missing it means the record shape drifted, which
        // is malformed.
        let note = parts
            .next()
            .ok_or_else(|| malformed_record(stripped))?
            .trim()
            .to_string();

        let change_id = change_id::extract_from_message(&message)
            .ok_or_else(|| {
                // Mirrors Python's `console_error` + INVALID_STATE
                // exit. The CLI surface (`_internal stack-local-commits`)
                // appends the helpful "did you run `mergify stack
                // setup`?" hint in the binary wrapper.
                CliError::InvalidState(format!(
                    "`Change-Id:` line is missing on commit {commit_sha}",
                ))
            })?
            .to_string();
        let slug = slug::slugify_title(&title, &change_id);

        out.push(LocalCommit {
            commit_sha,
            title,
            message,
            change_id,
            slug,
            note,
        });
    }
    Ok(out)
}

fn malformed_record(record: &str) -> CliError {
    CliError::Generic(format!("Unexpected git log record format: {record:?}"))
}

/// Type alias used by the binary wrapper when emitting the
/// `_internal stack-local-commits` JSON output.
pub type LocalCommits = Vec<LocalCommit>;

/// Resolve `repo_dir` argument. Defaults to the process CWD when
/// the caller doesn't pass an explicit value — the typical
/// invocation is from `mergify stack <cmd>` running inside the
/// user's clone.
#[must_use]
pub fn resolve_repo_dir(arg: Option<PathBuf>) -> PathBuf {
    arg.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(sha: &str, title: &str, body: &str) -> String {
        // Most parse tests don't care about the note field; default
        // to empty (the shape git emits for commits without a note
        // on the configured ref).
        record_with_note(sha, title, body, "")
    }

    fn record_with_note(sha: &str, title: &str, body: &str, note: &str) -> String {
        // Same wire shape git emits with our `--format` spec: NUL
        // between fields, RS terminator after the note.
        format!("{sha}\x00{title}\x00{body}\x00{note}\x1e")
    }

    #[test]
    fn parse_returns_commits_in_input_order() {
        // `--reverse` means oldest first; we just trust git's
        // ordering and check that we don't reorder it ourselves.
        let raw = record(
            "aaaa111111111111111111111111111111111111",
            "first",
            "body1\n\nChange-Id: Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ) + &record(
            "bbbb222222222222222222222222222222222222",
            "second",
            "body2\n\nChange-Id: Ibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        );
        let commits = parse(&raw).unwrap();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].title, "first");
        assert_eq!(
            commits[0].commit_sha,
            "aaaa111111111111111111111111111111111111"
        );
        assert_eq!(
            commits[0].change_id,
            "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(commits[1].title, "second");
        assert_eq!(
            commits[1].change_id,
            "Ibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
    }

    #[test]
    fn parse_skips_empty_records_at_boundary() {
        // A trailing `\x1e` after the last record produces an
        // empty split fragment — mirror Python's `stripped: continue`
        // skip.
        let raw = record(
            "aaaa111111111111111111111111111111111111",
            "only",
            "Change-Id: Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        );
        let commits = parse(&raw).unwrap();
        assert_eq!(commits.len(), 1);
    }

    #[test]
    fn parse_returns_empty_on_empty_input() {
        // `git log` on a no-op range emits nothing. Stack walks
        // should treat that as an empty result rather than an
        // error.
        assert!(parse("").unwrap().is_empty());
        assert!(parse("\x1e\x1e").unwrap().is_empty());
    }

    #[test]
    fn parse_picks_last_trailer_when_message_has_multiple() {
        // Amends sometimes append a new `Change-Id:` instead of
        // replacing the old one; the most recent one is the live
        // identity.
        let raw = record(
            "aaaa111111111111111111111111111111111111",
            "amended",
            "body\n\nChange-Id: I1111111111111111111111111111111111111111\nChange-Id: I2222222222222222222222222222222222222222",
        );
        let commits = parse(&raw).unwrap();
        assert_eq!(
            commits[0].change_id,
            "I2222222222222222222222222222222222222222"
        );
    }

    #[test]
    fn parse_rejects_record_missing_a_field() {
        // A three-field record (missing the note field) means our
        // format spec drifted from what git emits — surfacing it
        // loudly is more useful than silently dropping the
        // commit. We expect 4 NUL-separated fields: SHA, title,
        // body, note (note may be empty).
        let raw = "aaaa111111111111111111111111111111111111\x00title\x00body\x1e";
        let err = parse(raw).unwrap_err();
        match err {
            CliError::Generic(msg) => assert!(msg.contains("Unexpected git log record format")),
            other => panic!("expected Generic, got: {other:?}"),
        }
    }

    #[test]
    fn parse_extracts_note_when_present() {
        // The walker piggy-backs `git notes --ref=refs/notes/mergify/stack`
        // onto the same `git log` call by appending `%N` to the
        // format spec. Confirm a commit with a note surfaces it
        // verbatim on the `LocalCommit.note` field — that's how
        // the revision-history rendering will receive it without
        // a per-commit subprocess round-trip.
        let raw = record_with_note(
            "aaaa111111111111111111111111111111111111",
            "amend",
            "body\n\nChange-Id: Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "address review feedback",
        );
        let commits = parse(&raw).unwrap();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].note, "address review feedback");
    }

    #[test]
    fn parse_returns_empty_note_when_absent() {
        // Commits without a note still need a deterministic shape
        // (the `%N` field is empty, but git still emits the NUL
        // separator before the RS terminator). `record` defaults
        // to empty note, exercising exactly this case.
        let raw = record(
            "aaaa111111111111111111111111111111111111",
            "no-note",
            "Change-Id: Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        );
        let commits = parse(&raw).unwrap();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].note, "");
    }

    #[test]
    fn parse_returns_invalid_state_when_change_id_missing() {
        // The Python check exits with INVALID_STATE so the CLI
        // can prompt the user to run `mergify stack setup`. We
        // mirror the exit-code mapping so the user sees the same
        // signal regardless of which side raised it.
        let raw = record(
            "aaaa111111111111111111111111111111111111",
            "broken",
            "body with no trailer",
        );
        let err = parse(&raw).unwrap_err();
        match err {
            CliError::InvalidState(msg) => {
                assert!(msg.contains("Change-Id"));
                assert!(
                    msg.contains("aaaa111111111111111111111111111111111111"),
                    "missing-Change-Id error must name the offending commit so the user can fix it: {msg}",
                );
            }
            other => panic!("expected InvalidState, got: {other:?}"),
        }
    }

    #[test]
    fn read_walks_a_real_repository() {
        // End-to-end smoke that the `git -C <dir> log ...`
        // invocation, the format spec, and our parser all agree
        // on the wire shape — the unit `parse` tests can't catch
        // a drift between our `--format` string and what git
        // actually emits.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path();
        let git = |args: &[&str]| {
            let out = crate::test_env::isolated_git()
                .arg("-C")
                .arg(path)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
            String::from_utf8(out.stdout).unwrap()
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Tester"]);
        git(&["commit", "--allow-empty", "-m", "base\n"]);
        let base = git(&["rev-parse", "HEAD"]).trim().to_string();
        git(&[
            "commit",
            "--allow-empty",
            "-m",
            "first\n\nChange-Id: Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ]);
        git(&[
            "commit",
            "--allow-empty",
            "-m",
            "second\n\nChange-Id: Ibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ]);
        // Attach an amend-reason note to the second commit only,
        // so the assertions below can verify both branches —
        // `note == "address review feedback"` for the second,
        // `note == ""` for the first.
        let second_sha = git(&["rev-parse", "HEAD"]).trim().to_string();
        git(&[
            "notes",
            &format!("--ref={STACK_NOTES_REF}"),
            "add",
            "-m",
            "address review feedback",
            &second_sha,
        ]);

        let commits = read(path, &base, "HEAD").unwrap();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].title, "first");
        assert_eq!(commits[1].title, "second");
        assert_eq!(
            commits[0].change_id,
            "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        // First commit has no note on the stack ref → empty
        // string. Second has the note we just attached.
        assert_eq!(commits[0].note, "");
        assert_eq!(commits[1].note, "address review feedback");
    }
}
