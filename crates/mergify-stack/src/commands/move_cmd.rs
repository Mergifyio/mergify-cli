//! `mergify stack move <COMMIT> <POSITION> [<TARGET>] [--dry-run]`
//! — reposition a single commit within the stack. Wraps the
//! reorder machinery: compute the new order from the current
//! stack + the requested position, then delegate to the same
//! `Action::Reorder` path.
//!
//! Port of `mergify_cli/stack/move.py::stack_move`.

use std::path::Path;

use mergify_core::CliError;

use crate::change_id;
use crate::git::{resolve_repo_toplevel, run_git_capture, shell_quote, spawn_rebase};
use crate::local_commits::{self, LocalCommit};
use crate::plan_display::PlanRow;
use crate::trunk;

/// Where to move the commit. Mirrors Python's positional value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Position {
    First,
    Last,
    Before,
    After,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OrderedCommit {
    sha: String,
    subject: String,
    change_id: String,
}

#[derive(Debug, Clone)]
pub enum Outcome {
    /// Move ran to completion. `plan` is the new full-stack order
    /// (tag-less, like Python's `display_plan`).
    Moved {
        plan: Vec<PlanRow>,
    },
    /// `--dry-run` short-circuit. Same full-stack `plan`, no rebase.
    DryRun {
        plan: Vec<PlanRow>,
    },
    AlreadyInPosition,
    EmptyStack,
}

pub struct Options<'a> {
    pub repo_dir: Option<&'a Path>,
    pub commit_prefix: &'a str,
    pub position: Position,
    /// Required when `position` is `Before` / `After`; must be
    /// `None` for `First` / `Last`.
    pub target_prefix: Option<&'a str>,
    pub dry_run: bool,
    pub mergify_binary: &'a Path,
}

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

    let commit = match_commit(opts.commit_prefix, &commits)?;

    let target = match (&opts.position, opts.target_prefix) {
        (Position::Before | Position::After, None) => {
            return Err(CliError::InvalidState(format!(
                "'{name}' requires a target commit",
                name = position_name(&opts.position)
            )));
        }
        (Position::Before | Position::After, Some(prefix)) => {
            let resolved = match_commit(prefix, &commits)?;
            if resolved.sha == commit.sha {
                return Err(CliError::InvalidState(
                    "commit and target are the same".to_string(),
                ));
            }
            Some(resolved)
        }
        (Position::First | Position::Last, Some(_)) => {
            return Err(CliError::InvalidState(format!(
                "'{name}' does not accept a target commit",
                name = position_name(&opts.position)
            )));
        }
        (Position::First | Position::Last, None) => None,
    };

    let new_order = compute_new_order(&commits, &commit, &opts.position, target.as_ref());

    let current_shas: Vec<&str> = commits.iter().map(|c| c.commit_sha.as_str()).collect();
    let new_shas: Vec<&str> = new_order.iter().map(|c| c.sha.as_str()).collect();
    if current_shas == new_shas {
        return Ok(Outcome::AlreadyInPosition);
    }

    let plan: Vec<PlanRow> = new_order
        .iter()
        .map(|c| PlanRow {
            sha: c.sha.clone(),
            subject: c.subject.clone(),
            change_id: c.change_id.clone(),
            action: None,
        })
        .collect();

    if opts.dry_run {
        return Ok(Outcome::DryRun { plan });
    }

    spawn_reorder_rebase(&repo_dir, &base, opts.mergify_binary, &new_shas)?;
    Ok(Outcome::Moved { plan })
}

/// Build the reordered stack: every commit except `commit` in its
/// original order, with `commit` reinserted at the requested
/// `position` (relative to `target` for `Before` / `After`).
fn compute_new_order(
    commits: &[LocalCommit],
    commit: &OrderedCommit,
    position: &Position,
    target: Option<&OrderedCommit>,
) -> Vec<OrderedCommit> {
    let remaining: Vec<OrderedCommit> = commits
        .iter()
        .filter(|c| c.commit_sha != commit.sha)
        .map(|c| OrderedCommit {
            sha: c.commit_sha.clone(),
            subject: c.title.clone(),
            change_id: c.change_id.clone(),
        })
        .collect();

    match position {
        Position::First => {
            let mut v = Vec::with_capacity(remaining.len() + 1);
            v.push(commit.clone());
            v.extend(remaining);
            v
        }
        Position::Last => {
            let mut v = remaining;
            v.push(commit.clone());
            v
        }
        Position::Before | Position::After => {
            let target = target.expect("target validated above");
            let idx = remaining
                .iter()
                .position(|c| c.sha == target.sha)
                .expect("target was in stack");
            // `Before` inserts at the target's index; `After`
            // inserts just past it.
            let insert_at = if matches!(position, Position::After) {
                idx + 1
            } else {
                idx
            };
            let mut v = Vec::with_capacity(remaining.len() + 1);
            v.extend(remaining[..insert_at].iter().cloned());
            v.push(commit.clone());
            v.extend(remaining[insert_at..].iter().cloned());
            v
        }
    }
}

fn position_name(p: &Position) -> &'static str {
    match p {
        Position::First => "first",
        Position::Last => "last",
        Position::Before => "before",
        Position::After => "after",
    }
}

fn match_commit(prefix: &str, commits: &[LocalCommit]) -> Result<OrderedCommit, CliError> {
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
        [only] => Ok(OrderedCommit {
            sha: only.commit_sha.clone(),
            subject: only.title.clone(),
            change_id: only.change_id.clone(),
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

fn spawn_reorder_rebase(
    repo_dir: &Path,
    base: &str,
    mergify_binary: &Path,
    ordered_shas: &[&str],
) -> Result<(), CliError> {
    let bin = shell_quote(&mergify_binary.to_string_lossy());
    let shas = shell_quote(&ordered_shas.join(","));
    let editor = format!("{bin} _internal rebase-todo-rewrite --action reorder --shas {shas}");
    spawn_rebase(repo_dir, base, Some(&editor), false)
}
