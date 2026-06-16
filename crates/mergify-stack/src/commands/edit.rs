//! `mergify stack edit [<commit>]` — pause an interactive rebase
//! at a specific commit (or open the rebase interactively when
//! `<commit>` is omitted) so the user can `git commit --amend`
//! the target without leaving an editor running for the whole
//! todo list.
//!
//! Port of `mergify_cli/stack/edit.py::stack_edit`. The
//! non-interactive path uses [`crate::rebase_todo`] via the
//! binary's `_internal rebase-todo-rewrite` subcommand, set as
//! `GIT_SEQUENCE_EDITOR` before spawning `git rebase -i <base>`.

use std::path::Path;

use mergify_core::CliError;

use crate::change_id;
use crate::git::{resolve_repo_toplevel, run_git_capture, shell_quote, spawn_rebase};
use crate::local_commits;
use crate::trunk;

/// One commit in the stack — what `match_commit` picks out of
/// the local walker result. Returned by [`run`] for the
/// non-interactive path so callers can render
/// `Editing commit: <sha> <subject>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditingCommit {
    pub sha: String,
    pub subject: String,
}

/// Result of [`run`]. The two variants mirror the Python flow:
/// no commit prefix → interactive rebase (we just wait for the
/// editor); with prefix → scripted rebase pauses at the target.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// `mergify stack edit` (no argument) — interactive rebase
    /// returned. Nothing more for the caller to print.
    InteractiveCompleted,
    /// `mergify stack edit <prefix>` — the rebase is paused at
    /// `commit`; the caller prints the editing notice and an
    /// "amend then continue" hint.
    PausedAt { commit: EditingCommit },
    /// Stack is empty (`<merge-base trunk HEAD>..HEAD` returns
    /// no commits). Python prints `No commits in the stack` and
    /// returns 0.
    EmptyStack,
}

/// Parameters surfaced to the binary handler. `mergify_binary`
/// must point at the running binary so the spawned `git rebase`
/// can call back into `_internal rebase-todo-rewrite`.
pub struct Options<'a> {
    pub repo_dir: Option<&'a Path>,
    pub commit_prefix: Option<&'a str>,
    pub mergify_binary: &'a Path,
}

/// Resolve the trunk, compute the merge-base, then either spawn
/// `git rebase -i <base>` directly (interactive) or with a
/// `GIT_SEQUENCE_EDITOR` that marks the target commit as `edit`.
///
/// Errors:
/// - [`CliError::StackNotFound`] for an unresolved trunk or a
///   commit prefix that doesn't match any stack commit.
/// - [`CliError::InvalidState`] for an ambiguous Change-Id
///   prefix that matches more than one commit (consistent with
///   `mergify stack note`'s behavior).
/// - [`CliError::Generic`] for git invocation failures.
pub fn run(opts: &Options<'_>) -> Result<Outcome, CliError> {
    let repo_dir = resolve_repo_toplevel(opts.repo_dir)?;
    let trunk = trunk::get_trunk(Some(&repo_dir)).map_err(|e| {
        CliError::StackNotFound(format!(
            "could not determine trunk branch ({e}). Please set \
             upstream tracking or set a base manually."
        ))
    })?;
    let base = run_git_capture(Some(&repo_dir), &["merge-base", &trunk.refspec(), "HEAD"])?;

    let Some(commit_prefix) = opts.commit_prefix else {
        spawn_rebase(&repo_dir, &base, None)?;
        return Ok(Outcome::InteractiveCompleted);
    };

    let commits = local_commits::read(&repo_dir, &base, "HEAD")?;
    if commits.is_empty() {
        return Ok(Outcome::EmptyStack);
    }

    let target = match_commit(commit_prefix, &commits)?;
    let editor = build_sequence_editor(opts.mergify_binary, &target.sha);
    spawn_rebase(&repo_dir, &base, Some(&editor))?;

    Ok(Outcome::PausedAt { commit: target })
}

fn match_commit(
    prefix: &str,
    commits: &[local_commits::LocalCommit],
) -> Result<EditingCommit, CliError> {
    let (matches, field): (Vec<&local_commits::LocalCommit>, &str) = if change_id::is_prefix(prefix)
    {
        (
            commits
                .iter()
                .filter(|c| c.change_id.starts_with(prefix))
                .collect(),
            "Change-Id",
        )
    } else {
        (
            commits
                .iter()
                .filter(|c| c.commit_sha.starts_with(prefix))
                .collect(),
            "SHA",
        )
    };
    match matches.as_slice() {
        [] => Err(crate::match_commit::not_found(field, prefix)),
        [only] => Ok(EditingCommit {
            sha: only.commit_sha.clone(),
            subject: only.title.clone(),
        }),
        many => {
            let candidates: Vec<crate::match_commit::Candidate<'_>> = many
                .iter()
                .map(|c| crate::match_commit::Candidate {
                    commit_sha: &c.commit_sha,
                    title: &c.title,
                    change_id: &c.change_id,
                })
                .collect();
            Err(crate::match_commit::ambiguous(field, prefix, &candidates))
        }
    }
}

fn build_sequence_editor(binary: &Path, sha: &str) -> String {
    let bin = shell_quote(&binary.to_string_lossy());
    let sha = shell_quote(sha);
    format!("{bin} _internal rebase-todo-rewrite --action edit --sha {sha}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn build_stack_repo() -> (TempDir, Vec<(String, String)>) {
        // Bare upstream + local clone, three commits each carrying
        // a Change-Id trailer (the local_commits walker requires
        // it). Returns (workdir, [(sha, change_id), ...]).
        let workdir = tempfile::tempdir().unwrap();
        let upstream = workdir.path().join("up.git");
        crate::test_env::isolated_git()
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
        let mut commits = Vec::new();
        for (label, cid_hex) in [
            ("A", "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01"),
            ("B", "Ibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb02"),
            ("C", "Icccccccccccccccccccccccccccccccccccccc03"),
        ] {
            let msg = format!("Commit {label}\n\nChange-Id: {cid_hex}");
            run_in(&local, &["commit", "--allow-empty", "-m", &msg]);
            let sha = capture(&local, &["rev-parse", "HEAD"]);
            commits.push((sha, cid_hex.to_string()));
        }
        (workdir, commits)
    }

    fn run_in(dir: &Path, args: &[&str]) {
        let ok = crate::test_env::isolated_git()
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git -C {dir:?} {args:?} failed");
    }

    fn capture(dir: &Path, args: &[&str]) -> String {
        let out = crate::test_env::isolated_git()
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn match_commit_resolves_by_sha_prefix() {
        let (work, commits) = build_stack_repo();
        let local = work.path().join("local");
        let walker = local_commits::read(
            &local,
            &capture(&local, &["merge-base", "origin/main", "HEAD"]),
            "HEAD",
        )
        .unwrap();
        let target = match_commit(&commits[1].0[..7], &walker).unwrap();
        assert_eq!(target.sha, commits[1].0);
        assert_eq!(target.subject, "Commit B");
    }

    #[test]
    fn match_commit_resolves_by_change_id_prefix() {
        let (work, commits) = build_stack_repo();
        let local = work.path().join("local");
        let walker = local_commits::read(
            &local,
            &capture(&local, &["merge-base", "origin/main", "HEAD"]),
            "HEAD",
        )
        .unwrap();
        let target = match_commit(&commits[1].1[..9], &walker).unwrap();
        assert_eq!(target.sha, commits[1].0);
    }

    #[test]
    fn match_commit_unknown_prefix_returns_stack_not_found() {
        let (work, _) = build_stack_repo();
        let local = work.path().join("local");
        let walker = local_commits::read(
            &local,
            &capture(&local, &["merge-base", "origin/main", "HEAD"]),
            "HEAD",
        )
        .unwrap();
        let err = match_commit("deadbeef1234", &walker).unwrap_err();
        match err {
            CliError::StackNotFound(msg) => {
                assert!(msg.contains("deadbeef1234"), "got: {msg}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    // Note: end-to-end tests that actually spawn `git rebase -i`
    // with `GIT_SEQUENCE_EDITOR` pointing at the real `mergify`
    // binary live under `crates/mergify-cli/tests/stack_edit.rs`,
    // where `CARGO_BIN_EXE_mergify` is set by cargo. The tests
    // here cover the pure pieces that don't need the binary on
    // disk (commit matching, repo-toplevel resolution).
}
