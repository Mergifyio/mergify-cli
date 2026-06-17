//! `mergify stack reword <COMMIT> [-m <msg>] [--dry-run]` —
//! change a commit's message in place.
//!
//! Port of `mergify_cli/stack/reword.py::stack_reword`. Two
//! flavors:
//!
//! - **No `-m`**: marks the target as `reword` in the rebase-todo.
//!   Git stops at that commit and runs `git commit --amend`,
//!   opening `$GIT_EDITOR`. Works in a TTY, hangs in agent
//!   contexts.
//! - **With `-m`**: writes the message to a tempfile and injects
//!   `exec git commit --amend -F <file>` right after the target
//!   `pick`. The amend runs while HEAD points at the target, so
//!   any `prepare-commit-msg` hook re-attaches the Change-Id.
//!   The tempfile is intentionally leaked: if the rebase pauses
//!   on a conflict, `git rebase --continue` needs the file to
//!   complete the `exec`. The OS cleans up `/tmp` on its own.

use std::io::Write;
use std::path::{Path, PathBuf};

use mergify_core::CliError;

use crate::change_id;
use crate::git::{resolve_repo_toplevel, run_git_capture, shell_quote, spawn_rebase};
use crate::local_commits::{self, LocalCommit};
use crate::plan_display::PlanRow;
use crate::trunk;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewordedCommit {
    pub sha: String,
    pub subject: String,
}

#[derive(Debug, Clone)]
pub enum Outcome {
    /// Reword ran to completion. `plan` is the full stack in
    /// `base..HEAD` order with a `[reword]`/`[amend]` tag on the
    /// target row.
    Reworded {
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
    pub commit_prefix: &'a str,
    /// `Some` for the non-interactive `-m` path; `None` lets git
    /// open `$GIT_EDITOR` via the `reword` todo verb.
    pub message: Option<&'a str>,
    pub dry_run: bool,
    pub mergify_binary: &'a Path,
}

/// Resolve the trunk, walk the stack, match the commit, and run
/// the scripted rebase (or short-circuit on `--dry-run` /
/// empty-stack).
pub fn run(opts: &Options<'_>) -> Result<Outcome, CliError> {
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

    let target = match_commit(opts.commit_prefix, &commits)?;

    // No `-m` opens an editor (`reword` verb); `-m` amends the
    // message non-interactively (`amend`).
    let action = if opts.message.is_some() {
        "amend"
    } else {
        "reword"
    };
    let plan: Vec<PlanRow> = commits
        .iter()
        .map(|c| PlanRow {
            sha: c.commit_sha.clone(),
            subject: c.title.clone(),
            change_id: c.change_id.clone(),
            action: (c.commit_sha == target.sha).then(|| action.to_string()),
        })
        .collect();

    if opts.dry_run {
        return Ok(Outcome::DryRun { plan });
    }

    let editor = if let Some(msg) = opts.message {
        // Leak intentionally so `git rebase --continue` can still
        // find the file on conflict.
        let msg_path = write_temp_message(msg)?;
        let command = format!(
            "git commit --amend -F {}",
            shell_quote(&msg_path.to_string_lossy())
        );
        build_sequence_editor_exec(opts.mergify_binary, &target.sha, &command)
    } else {
        build_sequence_editor_reword(opts.mergify_binary, &target.sha)
    };
    spawn_rebase(&repo_dir, &base, Some(&editor))?;
    Ok(Outcome::Reworded { plan })
}

fn write_temp_message(message: &str) -> Result<PathBuf, CliError> {
    // `into_temp_path` returns a path the caller owns; `keep`
    // converts it into a regular path that persists past the
    // tempfile's lifetime. Matches Python's intentional leak.
    let mut tmp = tempfile::Builder::new()
        .prefix("mergify_reword_msg_")
        .suffix(".txt")
        .tempfile()
        .map_err(|e| CliError::Generic(format!("create reword tempfile: {e}")))?;
    tmp.write_all(message.as_bytes())
        .map_err(|e| CliError::Generic(format!("write reword tempfile: {e}")))?;
    tmp.flush()
        .map_err(|e| CliError::Generic(format!("flush reword tempfile: {e}")))?;
    let (_, path) = tmp
        .keep()
        .map_err(|e| CliError::Generic(format!("persist reword tempfile: {e}")))?;
    Ok(path)
}

fn match_commit(prefix: &str, commits: &[LocalCommit]) -> Result<RewordedCommit, CliError> {
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
        [only] => Ok(RewordedCommit {
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

fn build_sequence_editor_reword(binary: &Path, sha: &str) -> String {
    let bin = shell_quote(&binary.to_string_lossy());
    let sha = shell_quote(sha);
    format!("{bin} _internal rebase-todo-rewrite --action reword --sha {sha}")
}

fn build_sequence_editor_exec(binary: &Path, sha: &str, command: &str) -> String {
    let bin = shell_quote(&binary.to_string_lossy());
    let sha = shell_quote(sha);
    let command = shell_quote(command);
    format!(
        "{bin} _internal rebase-todo-rewrite --action exec-after --sha {sha} --command {command}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_repo() -> (tempfile::TempDir, Vec<String>) {
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
            crate::test_env::isolated_git()
                .arg("-C")
                .arg(&local)
                .args(args)
                .status()
                .unwrap();
        }
        let mut commits = Vec::new();
        for (label, cid) in [
            ("A", "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01"),
            ("B", "Ibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb02"),
        ] {
            let msg = format!("Commit {label}\n\nChange-Id: {cid}");
            crate::test_env::isolated_git()
                .arg("-C")
                .arg(&local)
                .args(["commit", "--allow-empty", "-m", &msg])
                .status()
                .unwrap();
            let out = crate::test_env::isolated_git()
                .arg("-C")
                .arg(&local)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap();
            commits.push(String::from_utf8(out.stdout).unwrap().trim().to_string());
        }
        (workdir, commits)
    }

    #[test]
    fn dry_run_returns_target() {
        let (work, commits) = build_repo();
        let local = work.path().join("local");
        let outcome = run(&Options {
            repo_dir: Some(&local),
            commit_prefix: &commits[1][..12],
            message: None,
            dry_run: true,
            mergify_binary: Path::new("does-not-matter"),
        })
        .unwrap();
        match outcome {
            Outcome::DryRun { plan } => {
                // Full stack (A, B); B is the reworded target.
                assert_eq!(plan.len(), 2);
                assert_eq!(plan[1].sha, commits[1]);
                assert_eq!(plan[1].subject, "Commit B");
                assert_eq!(plan[1].action.as_deref(), Some("reword"));
                assert_eq!(plan[0].action, None);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn dry_run_with_message_signals_amend() {
        let (work, commits) = build_repo();
        let local = work.path().join("local");
        let outcome = run(&Options {
            repo_dir: Some(&local),
            commit_prefix: &commits[1][..12],
            message: Some("new subject"),
            dry_run: true,
            mergify_binary: Path::new("does-not-matter"),
        })
        .unwrap();
        match outcome {
            Outcome::DryRun { plan } => {
                // `-m` tags the target as `amend` rather than `reword`.
                assert_eq!(plan[1].action.as_deref(), Some("amend"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn unknown_prefix_returns_stack_not_found() {
        let (work, _) = build_repo();
        let local = work.path().join("local");
        let err = run(&Options {
            repo_dir: Some(&local),
            commit_prefix: "deadbeef1234",
            message: None,
            dry_run: true,
            mergify_binary: Path::new("does-not-matter"),
        })
        .unwrap_err();
        match err {
            CliError::StackNotFound(_) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
}
