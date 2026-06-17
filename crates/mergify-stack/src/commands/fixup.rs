//! `mergify stack fixup <COMMIT>... [--dry-run]` — fold one or
//! more commits into their parents in the stack.
//!
//! Port of `mergify_cli/stack/squash.py::stack_fixup`. Shares the
//! rebase-todo machinery with `stack drop` and `stack edit`: same
//! orchestrator shape, the only difference vs. drop is that this
//! command rewrites the targeted `pick` lines as `fixup` (preserving
//! the change so it folds into the previous commit) rather than
//! removing them outright.
//!
//! One extra validation specific to fixup: the first commit of the
//! stack has no parent inside the stack, so fixing it up would
//! collapse it into the trunk's parent. Python rejects this with
//! `INVALID_STATE`; we mirror the check.

use std::path::Path;

use mergify_core::CliError;

use crate::change_id;
use crate::git::{resolve_repo_toplevel, run_git_capture, shell_quote, spawn_rebase};
use crate::local_commits::{self, LocalCommit};
use crate::plan_display::PlanRow;
use crate::trunk;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedUpCommit {
    pub sha: String,
    pub subject: String,
}

#[derive(Debug, Clone)]
pub enum Outcome {
    /// Fixup ran to completion. `plan` is the full stack in
    /// `base..HEAD` order with `[fixup]` tags on the folded rows.
    Squashed {
        plan: Vec<PlanRow>,
    },
    /// `--dry-run` short-circuit. Same full-stack `plan`, no rebase.
    DryRun {
        plan: Vec<PlanRow>,
    },
    EmptyStack,
}

pub struct Options<'a> {
    pub repo_dir: Option<&'a Path>,
    pub commit_prefixes: &'a [String],
    pub dry_run: bool,
    pub mergify_binary: &'a Path,
}

/// Resolve the trunk, walk the stack, match each `<COMMIT>`
/// argument, ensure none of them is the first stack commit, then
/// spawn the scripted rebase.
///
/// Errors:
/// - [`CliError::StackNotFound`] for an unresolved trunk or a
///   commit prefix that doesn't match any stack commit.
/// - [`CliError::InvalidState`] for a duplicate prefix (two
///   `<COMMIT>` args resolving to the same commit), an ambiguous
///   Change-Id prefix, or targeting the first commit of the stack.
/// - [`CliError::Generic`] for git invocation failures.
pub fn run(opts: &Options<'_>) -> Result<Outcome, CliError> {
    if opts.commit_prefixes.is_empty() {
        return Err(CliError::InvalidState("no commits to fixup".to_string()));
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

    let first_sha = &commits[0].commit_sha;

    let mut resolved = Vec::with_capacity(opts.commit_prefixes.len());
    let mut seen_shas = std::collections::HashSet::new();
    for prefix in opts.commit_prefixes {
        let matched = match_commit(prefix, &commits)?;
        if &matched.sha == first_sha {
            return Err(CliError::InvalidState(
                "cannot fixup the first commit of the stack — no parent in stack".to_string(),
            ));
        }
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
                .then(|| "fixup".to_string()),
        })
        .collect();

    if opts.dry_run {
        return Ok(Outcome::DryRun { plan });
    }

    let shas: Vec<String> = resolved.iter().map(|c| c.sha.clone()).collect();
    let editor = build_sequence_editor(opts.mergify_binary, &shas);
    spawn_rebase(&repo_dir, &base, Some(&editor))?;
    Ok(Outcome::Squashed { plan })
}

fn match_commit(prefix: &str, commits: &[LocalCommit]) -> Result<FixedUpCommit, CliError> {
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
        [] => Err(CliError::StackNotFound(format!(
            "no commit found matching {field} prefix '{prefix}'"
        ))),
        [only] => Ok(FixedUpCommit {
            sha: only.commit_sha.clone(),
            subject: only.title.clone(),
        }),
        many => {
            let listing = many
                .iter()
                .map(|c| format!("{} {}", &c.commit_sha[..7], c.title))
                .collect::<Vec<_>>()
                .join("\n  ");
            Err(CliError::InvalidState(format!(
                "{field} prefix '{prefix}' matches multiple commits:\n  {listing}"
            )))
        }
    }
}

fn build_sequence_editor(binary: &Path, shas: &[String]) -> String {
    let bin = shell_quote(&binary.to_string_lossy());
    let shas = shell_quote(&shas.join(","));
    format!("{bin} _internal rebase-todo-rewrite --action fixup --shas {shas}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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
        assert!(ok);
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
    fn first_commit_is_rejected() {
        let (work, commits) = build_stack_repo();
        let local = work.path().join("local");
        let err = run(&Options {
            repo_dir: Some(&local),
            commit_prefixes: &[commits[0].0[..12].to_string()],
            dry_run: true,
            mergify_binary: Path::new("does-not-matter"),
        })
        .unwrap_err();
        match err {
            CliError::InvalidState(msg) => {
                assert!(msg.contains("first commit"), "got: {msg}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn dry_run_resolves_without_spawning_rebase() {
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
                // tagged for fixup.
                assert_eq!(plan.len(), 3);
                assert_eq!(plan[1].sha, commits[1].0);
                assert_eq!(plan[1].subject, "Commit B");
                assert_eq!(plan[1].action.as_deref(), Some("fixup"));
                assert_eq!(plan[0].action, None);
                assert_eq!(plan[2].action, None);
            }
            other => panic!("unexpected: {other:?}"),
        }
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
                commits[1].0[..12].to_string(),
            ],
            dry_run: true,
            mergify_binary: Path::new("does-not-matter"),
        })
        .unwrap_err();
        match err {
            CliError::InvalidState(msg) => {
                assert!(msg.contains("duplicate"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
