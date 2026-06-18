//! `mergify stack drop <COMMIT>... [--dry-run]` — remove one or
//! more commits from the stack.
//!
//! Port of `mergify_cli/stack/drop.py::stack_drop`. Uses the
//! shared rebase-todo machinery added with `stack edit`: spawn
//! `git rebase -i <base>` with `GIT_SEQUENCE_EDITOR` pointing at
//! `mergify _internal rebase-todo-rewrite --action drop --shas
//! <SHA,SHA,…>`, which deletes the targeted `pick` lines from the
//! todo before git replays the rebase.

use std::path::Path;

use mergify_core::CliError;

use crate::change_id;
use crate::git::{resolve_repo_toplevel, run_git_capture, shell_quote, spawn_rebase};
use crate::local_commits::{self, LocalCommit};
use crate::plan_display::PlanRow;
use crate::trunk;

/// One commit the caller wants to drop, used for prefix
/// resolution before building the full-stack plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedCommit {
    pub sha: String,
    pub subject: String,
}

#[derive(Debug, Clone)]
pub enum Outcome {
    /// Drop ran to completion. `plan` is the full stack in
    /// `base..HEAD` order with `[drop]` tags on the dropped rows.
    Dropped { plan: Vec<PlanRow> },
    /// `--dry-run` short-circuit. Same full-stack `plan`, no rebase.
    DryRun { plan: Vec<PlanRow> },
    /// Stack is empty — Python prints `No commits in the stack`
    /// and exits 0.
    EmptyStack,
}

pub struct Options<'a> {
    pub repo_dir: Option<&'a Path>,
    pub commit_prefixes: &'a [String],
    pub dry_run: bool,
    pub mergify_binary: &'a Path,
}

/// Resolve the trunk, walk the stack, match each `<COMMIT>`
/// argument against the walker, then spawn the scripted rebase.
///
/// Errors:
/// - [`CliError::StackNotFound`] for an unresolved trunk or a
///   commit prefix that doesn't match any stack commit.
/// - [`CliError::InvalidState`] for a duplicate prefix (two
///   `<COMMIT>` args resolving to the same commit) or an
///   ambiguous Change-Id prefix that matches more than one.
/// - [`CliError::Generic`] for git invocation failures.
pub fn run(opts: &Options<'_>) -> Result<Outcome, CliError> {
    if opts.commit_prefixes.is_empty() {
        return Err(CliError::InvalidState("no commits to drop".to_string()));
    }
    let repo_dir = resolve_repo_toplevel(opts.repo_dir)?;
    let trunk = trunk::get_trunk(Some(&repo_dir)).map_err(|e| {
        CliError::StackNotFound(format!(
            "could not determine trunk branch ({e}). Please set \
             upstream tracking or set a base manually."
        ))
    })?;
    let base = run_git_capture(Some(&repo_dir), &["merge-base", &trunk.refspec(), "HEAD"])?;
    let commits = local_commits::read(&repo_dir, &base, "HEAD")?;
    if commits.is_empty() {
        return Ok(Outcome::EmptyStack);
    }

    let mut resolved = Vec::with_capacity(opts.commit_prefixes.len());
    let mut seen_shas = std::collections::HashSet::new();
    for prefix in opts.commit_prefixes {
        let matched = match_commit(prefix, &commits)?;
        if !seen_shas.insert(matched.sha.clone()) {
            return Err(CliError::InvalidState(format!(
                "duplicate — prefix '{prefix}' resolves to the same commit as another prefix"
            )));
        }
        resolved.push(matched);
    }

    let plan: Vec<PlanRow> = commits
        .iter()
        .map(|c| PlanRow {
            sha: c.commit_sha.clone(),
            subject: c.title.clone(),
            change_id: c.change_id.clone(),
            action: seen_shas
                .contains(&c.commit_sha)
                .then(|| "drop".to_string()),
        })
        .collect();

    if opts.dry_run {
        return Ok(Outcome::DryRun { plan });
    }

    let shas: Vec<String> = resolved.iter().map(|c| c.sha.clone()).collect();
    let editor = build_sequence_editor(opts.mergify_binary, &shas);
    spawn_rebase(&repo_dir, &base, Some(&editor))?;
    Ok(Outcome::Dropped { plan })
}

fn match_commit(prefix: &str, commits: &[LocalCommit]) -> Result<DroppedCommit, CliError> {
    let (matches, field): (Vec<&LocalCommit>, &str) = if change_id::is_prefix(prefix) {
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
        [only] => Ok(DroppedCommit {
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

fn build_sequence_editor(binary: &Path, shas: &[String]) -> String {
    let bin = shell_quote(&binary.to_string_lossy());
    let shas = shell_quote(&shas.join(","));
    format!("{bin} _internal rebase-todo-rewrite --action drop --shas {shas}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Bare upstream + local clone with three commits on `feature`
    /// (each carrying a Change-Id trailer).
    fn build_stack_repo() -> (TempDir, Vec<(String, String)>) {
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
    fn dry_run_resolves_commits_without_spawning_rebase() {
        let (work, commits) = build_stack_repo();
        let local = work.path().join("local");
        let outcome = run(&Options {
            repo_dir: Some(&local),
            commit_prefixes: &[commits[1].0[..12].to_string()],
            dry_run: true,
            mergify_binary: Path::new("does-not-matter"),
        })
        .unwrap();
        match outcome {
            Outcome::DryRun { plan } => {
                // Plan lists the whole stack (A, B, C); only B is
                // tagged for drop.
                assert_eq!(plan.len(), 3);
                assert_eq!(plan[1].sha, commits[1].0);
                assert_eq!(plan[1].subject, "Commit B");
                assert_eq!(plan[1].action.as_deref(), Some("drop"));
                assert_eq!(plan[0].action, None);
                assert_eq!(plan[2].action, None);
            }
            other => panic!("unexpected: {other:?}"),
        }
        // HEAD untouched.
        assert_eq!(capture(&local, &["log", "-1", "--format=%s"]), "Commit C");
    }

    #[test]
    fn duplicate_prefix_is_rejected() {
        let (work, commits) = build_stack_repo();
        let local = work.path().join("local");
        let err = run(&Options {
            repo_dir: Some(&local),
            commit_prefixes: &[
                commits[1].0[..7].to_string(),
                // Same commit, different prefix length.
                commits[1].0[..12].to_string(),
            ],
            dry_run: true,
            mergify_binary: Path::new("does-not-matter"),
        })
        .unwrap_err();
        match err {
            CliError::InvalidState(msg) => {
                assert!(msg.contains("duplicate"), "got: {msg}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn change_id_prefix_resolves_against_stack() {
        let (work, commits) = build_stack_repo();
        let local = work.path().join("local");
        let outcome = run(&Options {
            repo_dir: Some(&local),
            commit_prefixes: &[commits[2].1[..9].to_string()],
            dry_run: true,
            mergify_binary: Path::new("does-not-matter"),
        })
        .unwrap();
        match outcome {
            Outcome::DryRun { plan } => {
                // C (index 2 in the stack) is the change-id match
                // and the only tagged row.
                assert_eq!(plan[2].sha, commits[2].0);
                assert_eq!(plan[2].action.as_deref(), Some("drop"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn empty_stack_returns_empty_outcome() {
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
        let outcome = run(&Options {
            repo_dir: Some(&local),
            commit_prefixes: &["anything".to_string()],
            dry_run: false,
            mergify_binary: Path::new("does-not-matter"),
        })
        .unwrap();
        assert!(matches!(outcome, Outcome::EmptyStack));
    }

    #[test]
    fn unknown_prefix_returns_stack_not_found() {
        let (work, _) = build_stack_repo();
        let local = work.path().join("local");
        let err = run(&Options {
            repo_dir: Some(&local),
            commit_prefixes: &["deadbeef1234".to_string()],
            dry_run: false,
            mergify_binary: Path::new("does-not-matter"),
        })
        .unwrap_err();
        match err {
            CliError::StackNotFound(_) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
}
