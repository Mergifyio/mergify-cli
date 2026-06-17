//! `mergify stack reorder <COMMIT>... [--dry-run]` — rebase the
//! stack with the given commit order.
//!
//! Port of `mergify_cli/stack/reorder.py::stack_reorder`. The
//! `<COMMIT>...` arguments are SHA prefixes (or Change-Id
//! prefixes) — one per stack commit, in the desired new order.
//! Validation rejects duplicate prefixes, prefixes that don't
//! resolve, and length mismatches. When the requested order
//! matches the current order, we short-circuit with an "already
//! in order" outcome and don't spawn a rebase.

use std::path::Path;

use mergify_core::CliError;

use crate::change_id;
use crate::git::{resolve_repo_toplevel, run_git_capture, shell_quote, spawn_rebase};
use crate::local_commits::{self, LocalCommit};
use crate::plan_display::PlanRow;
use crate::trunk;

#[derive(Debug, Clone, PartialEq, Eq)]
struct OrderedCommit {
    sha: String,
    subject: String,
    change_id: String,
}

#[derive(Debug, Clone)]
pub enum Outcome {
    /// Reorder ran to completion. `plan` is the new full-stack
    /// order (tag-less, like Python's `display_plan`).
    Reordered {
        plan: Vec<PlanRow>,
    },
    /// `--dry-run` short-circuit. Same full-stack `plan`, no rebase.
    DryRun {
        plan: Vec<PlanRow>,
    },
    /// New order matches current order — no rebase needed.
    AlreadyInOrder,
    EmptyStack,
}

pub struct Options<'a> {
    pub repo_dir: Option<&'a Path>,
    pub commit_prefixes: &'a [String],
    pub dry_run: bool,
    pub mergify_binary: &'a Path,
}

pub fn run(opts: &Options<'_>) -> Result<Outcome, CliError> {
    if opts.commit_prefixes.is_empty() {
        return Err(CliError::InvalidState(
            "no commit prefixes given".to_string(),
        ));
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

    if opts.commit_prefixes.len() != commits.len() {
        return Err(CliError::InvalidState(format!(
            "expected {} commits but got {} prefixes",
            commits.len(),
            opts.commit_prefixes.len()
        )));
    }

    let mut resolved: Vec<OrderedCommit> = Vec::with_capacity(opts.commit_prefixes.len());
    let mut seen = std::collections::HashSet::new();
    for prefix in opts.commit_prefixes {
        let matched = match_commit(prefix, &commits)?;
        if !seen.insert(matched.sha.clone()) {
            return Err(CliError::InvalidState(format!(
                "duplicate — prefix '{prefix}' resolves to the same commit as another prefix"
            )));
        }
        resolved.push(matched);
    }

    let current_shas: Vec<&str> = commits.iter().map(|c| c.commit_sha.as_str()).collect();
    let requested_shas: Vec<&str> = resolved.iter().map(|c| c.sha.as_str()).collect();
    if current_shas == requested_shas {
        return Ok(Outcome::AlreadyInOrder);
    }

    let plan: Vec<PlanRow> = resolved
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

    spawn_reorder_rebase(&repo_dir, &base, opts.mergify_binary, &requested_shas)?;
    Ok(Outcome::Reordered { plan })
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
        [] => Err(CliError::StackNotFound(format!(
            "no commit found matching {field} prefix '{prefix}'"
        ))),
        [only] => Ok(OrderedCommit {
            sha: only.commit_sha.clone(),
            subject: only.title.clone(),
            change_id: only.change_id.clone(),
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

fn spawn_reorder_rebase(
    repo_dir: &Path,
    base: &str,
    mergify_binary: &Path,
    ordered_shas: &[&str],
) -> Result<(), CliError> {
    let bin = shell_quote(&mergify_binary.to_string_lossy());
    let shas = shell_quote(&ordered_shas.join(","));
    let editor = format!("{bin} _internal rebase-todo-rewrite --action reorder --shas {shas}");
    spawn_rebase(repo_dir, base, Some(&editor))
}
