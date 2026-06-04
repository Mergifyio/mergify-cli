//! Native `mergify stack push` orchestrator.
//!
//! Wires every leaf module ported from `mergify_cli/stack/` into
//! the end-to-end push flow:
//!
//! 1. Resolve git context (branch, repo, stack prefix).
//! 2. Fetch trunk + the `refs/notes/mergify/stack` ref.
//! 3. Walk local commits + remote PRs and plan the actions via
//!    [`crate::plan`].
//! 4. Decide rebase vs. skip via [`crate::approvals`].
//! 5. Rebase via [`crate::commands::sync`] (or skip).
//! 6. Optionally compute change types + replay SHAs for the
//!    revision-history comment.
//! 7. `git push --atomic` via [`crate::notes_push`].
//! 8. Upsert each PR sequentially via [`crate::pr_upsert`] so
//!    each `Depends-On: #<n>` header sees the predecessor's
//!    freshly-created PR number.
//! 9. Upsert stack comments + revision-history comments per PR
//!    via [`crate::comment_upsert`].
//! 10. Tear down orphan branches.
//!
//! Ported from `mergify_cli/stack/push.py::stack_push`. The
//! Python version fans out PR upserts through `asyncio.gather +
//! asyncio.Event` so a `Create` predecessor doesn't block its
//! dependent; the Rust port runs them sequentially because for
//! the typical 2–5 PR stacks the latency difference is dominated
//! by the GitHub round-trip anyway, and the simpler code avoids
//! a `tokio::sync::Notify` graph.

use std::path::Path;
use std::process::Command;

use chrono::Utc;
use mergify_core::{CliError, HttpClient};
use serde_json::Value;

use crate::approvals::{self, RebaseDecision};
use crate::change_type::{self, ChangeType};
use crate::changes::Action;
use crate::commands::sync as sync_cmd;
use crate::comment_upsert::{self, RevisionInput};
use crate::local_commits;
use crate::notes_push::{self, PushEntry};
use crate::plan::{self, PlannedChange, PlannedChanges, PlannerOpts};
use crate::pr_upsert::{self, PrUpsertInput};
use crate::rebase_log;
use crate::remote_changes;
use crate::replay;
use crate::stack_comment::StackEntry;
use crate::stack_context;
use crate::trunk;

/// Inputs for [`run`]. Mirrors the Python `stack_push` kwargs
/// plus the bits Python pulls from the click context (server,
/// token, trunk).
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors the Python CLI's flag surface 1:1"
)]
#[derive(Clone)]
pub struct Options<'a> {
    pub repo_dir: Option<&'a Path>,
    pub client: &'a HttpClient,
    pub mergify_binary: &'a Path,
    /// Base URL (e.g. `https://api.github.com`) used for
    /// revision-history compare URLs.
    pub github_server: &'a str,
    /// `(remote, branch)` of the trunk.
    pub trunk: (&'a str, &'a str),
    /// GitHub login of the author whose PRs the stack belongs
    /// to. Used both for the search filter in `remote_changes`
    /// and for `branch_prefix` fallback resolution.
    pub author: &'a str,
    /// PR branch prefix (typically `stack/<user>`); empty
    /// string means "use the dest branch name directly" (the
    /// Python `branch_prefix or dest_branch` fallback).
    pub branch_prefix: &'a str,
    pub user: &'a str,
    pub repo: &'a str,
    pub skip_rebase: bool,
    pub force_rebase: bool,
    pub next_only: bool,
    pub dry_run: bool,
    pub create_as_draft: bool,
    pub keep_pull_request_title_and_body: bool,
    pub only_update_existing_pulls: bool,
    pub revision_history: bool,
    pub no_verify: bool,
}

/// Outcome of [`run`]. `DryRun` carries the plan + the rebase
/// decision (the things the CLI's dry-run path renders) so the
/// caller can format the output. `Pushed` reports the per-PR
/// results so a future test harness can assert on them.
#[derive(Debug)]
pub enum Outcome {
    DryRun {
        planned: PlannedChanges,
        rebase: RebaseDecision,
        commits_behind: u32,
        log_lines: Vec<String>,
    },
    Pushed {
        planned: PlannedChanges,
        rebase: RebaseDecision,
        /// Per-PR upserted pull payloads (Create / Update only).
        upserted: Vec<Value>,
        log_lines: Vec<String>,
    },
}

/// Execute the orchestrator.
///
/// Returns `Ok` on a clean run (including dry-run). Errors
/// propagate as `CliError` — the binary's top-level error
/// handler maps them to exit codes.
#[allow(
    clippy::too_many_lines,
    reason = "single end-to-end flow that's clearer inline than split"
)]
pub async fn run(opts: &Options<'_>) -> Result<Outcome, CliError> {
    let repo_dir = resolve_repo_toplevel(opts.repo_dir)?;
    let dest_branch = trunk::git_get_branch_name(Some(&repo_dir))?;

    stack_context::check_local_branch(&dest_branch, opts.branch_prefix)?;

    let (remote, base_branch) = opts.trunk;
    if base_branch == dest_branch {
        return Err(CliError::InvalidState(format!(
            "your local branch `{dest_branch}` targets itself: \
             `{remote}/{base_branch}`. \
             Switch off the trunk branch before pushing.",
        )));
    }

    let stack_prefix = if opts.branch_prefix.is_empty() {
        dest_branch.clone()
    } else {
        format!("{prefix}/{dest_branch}", prefix = opts.branch_prefix)
    };

    // Pre-push: trunk fetch + notes fetch. Both are required
    // before merge-base / push can run.
    run_git_silent(&repo_dir, &["fetch", remote, base_branch])?;
    let notes_ref_fetched = notes_push::fetch_notes_ref(Some(&repo_dir), remote)?;

    let trunk_ref = format!("{remote}/{base_branch}");
    let base_commit_sha = compute_base_commit_sha(&repo_dir, &trunk_ref, &dest_branch)?;

    let remote_changes_data = remote_changes::get_remote_changes(
        opts.client,
        opts.user,
        opts.repo,
        &stack_prefix,
        opts.author,
    )
    .await?;
    let local = local_commits::read(&repo_dir, &base_commit_sha, "HEAD")?;

    let planner_opts = PlannerOpts {
        stack_prefix: &stack_prefix,
        base_branch,
        only_update_existing_pulls: opts.only_update_existing_pulls,
        next_only: opts.next_only,
    };
    let mut planned = plan::plan(&local, remote_changes_data.clone(), planner_opts)?;

    let rebase_decision = approvals::decide_rebase(
        opts.client,
        opts.user,
        opts.repo,
        &PlannedAsChanges(&planned).into(),
        opts.skip_rebase,
        opts.force_rebase,
    )
    .await?;

    let mut log_lines: Vec<String> = Vec::new();

    if opts.dry_run {
        let commits_behind = git_count_behind(&repo_dir, &trunk_ref)?;
        log_lines.extend(rebase_log::rebase_dry_run(
            &dest_branch,
            remote,
            base_branch,
            commits_behind,
            &rebase_decision,
        ));
        if rebase_decision.should_rebase && commits_behind > 0 {
            plan::promote_skip_up_to_date_to_update(&mut planned);
        }
        return Ok(Outcome::DryRun {
            planned,
            rebase: rebase_decision,
            commits_behind,
            log_lines,
        });
    }

    // Real-push path.
    if rebase_decision.should_rebase {
        let sync_opts = sync_cmd::Options {
            repo_dir: Some(&repo_dir),
            client: opts.client,
            user: opts.user,
            repo: opts.repo,
            author: opts.author,
            branch_prefix: opts.branch_prefix,
            trunk: opts.trunk,
            dry_run: false,
            mergify_binary: opts.mergify_binary,
        };
        let outcome = sync_cmd::run(&sync_opts).await?;
        let dropped = match outcome {
            sync_cmd::Outcome::Synced { dropped_count, .. } => dropped_count,
            sync_cmd::Outcome::DryRun(_) => 0,
        };
        log_lines.push(rebase_log::rebase_performed(
            &dest_branch,
            remote,
            base_branch,
            dropped,
            &rebase_decision,
        ));

        // Rebase changed local SHAs — recompute base + re-plan.
        let new_base = compute_base_commit_sha(&repo_dir, &trunk_ref, &dest_branch)?;
        let local2 = local_commits::read(&repo_dir, &new_base, "HEAD")?;
        planned = plan::plan(&local2, remote_changes_data, planner_opts)?;
    } else if let Some(line) = rebase_log::rebase_skipped(&dest_branch, &rebase_decision) {
        log_lines.push(line);
    }

    // Pre-push: optional change-type detection for the
    // revision-history comment. Order matters — must happen
    // before `push_branches` overwrites the remote refs.
    let mut change_types: std::collections::HashMap<String, ChangeType> =
        std::collections::HashMap::new();
    if opts.revision_history {
        let updated_pr_numbers: Vec<u64> = planned
            .locals
            .iter()
            .filter(|p| matches!(p.change.action, Action::Update))
            .filter_map(|p| {
                p.change
                    .pull
                    .as_ref()
                    .and_then(|v| v.get("number"))
                    .and_then(Value::as_u64)
            })
            .collect();
        if let Err(e) =
            change_type::fetch_old_pr_heads(Some(&repo_dir), remote, &updated_pr_numbers)
        {
            log_lines.push(format!(
                "[orange]Could not fetch old PR heads; revision-history \
                 change types will fall back to 'unknown': {e}[/]",
            ));
        }
        for entry in &planned.locals {
            if !matches!(entry.change.action, Action::Update) {
                continue;
            }
            let Some(pull) = entry.change.pull.as_ref() else {
                continue;
            };
            let Some(old_sha) = pull.pointer("/head/sha").and_then(Value::as_str) else {
                continue;
            };
            let ct =
                change_type::detect_change_type(Some(&repo_dir), old_sha, &entry.change.commit_sha);
            change_types.insert(entry.change.change_id.clone(), ct);
        }
    }

    // The actual push.
    let push_entries: Vec<PushEntry> = planned
        .locals
        .iter()
        .filter(|p| matches!(p.change.action, Action::Create | Action::Update))
        .map(|p| {
            let pull_head_sha = if matches!(p.change.action, Action::Update) {
                p.change
                    .pull
                    .as_ref()
                    .and_then(|v| v.pointer("/head/sha"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            } else {
                None
            };
            PushEntry {
                commit_sha: p.change.commit_sha.clone(),
                dest_branch: p.dest_branch.clone(),
                pull_head_sha,
            }
        })
        .collect();
    notes_push::push_branches(
        Some(&repo_dir),
        remote,
        &push_entries,
        opts.no_verify,
        notes_ref_fetched,
    )?;

    // Sequential per-PR upsert so each Depends-On has access to
    // the predecessor's freshly-known PR number. Python uses
    // asyncio fan-out + Event coordination; sequential is fine
    // for typical stack sizes.
    let mut upserted: Vec<Value> = Vec::new();
    let mut last_pull_number: Option<u64> = None;
    for entry in &mut planned.locals {
        let action = entry.change.action;
        if !matches!(action, Action::Create | Action::Update) {
            // Skip-* still carries an existing pull number
            // forward as a potential predecessor for downstream
            // Depends-On chaining. Mirrors Python's
            // `_build_change_tasks` carry-forward semantics.
            if let Some(n) = entry
                .change
                .pull
                .as_ref()
                .and_then(|v| v.get("number"))
                .and_then(Value::as_u64)
            {
                last_pull_number = Some(n);
            }
            continue;
        }

        let input = PrUpsertInput {
            action,
            title: &entry.change.title,
            message: &entry.change.message,
            dest_branch: &entry.dest_branch,
            base_branch: &entry.base_branch,
            pull: entry.change.pull.as_ref(),
            depends_on_number: last_pull_number,
            create_as_draft: opts.create_as_draft,
            keep_pull_request_title_and_body: opts.keep_pull_request_title_and_body,
        };
        let pull = pr_upsert::create_or_update_pr(opts.client, opts.user, opts.repo, input).await?;
        if let Some(n) = pull.get("number").and_then(Value::as_u64) {
            last_pull_number = Some(n);
        }
        entry.change.pull = Some(pull.clone());
        upserted.push(pull);
    }

    // Stack comments (only when stack has > 1 PR — the upserter
    // also guards on this but we pre-filter to avoid a useless
    // GET when total_pulls == 1).
    let entries: Vec<StackEntry> = planned
        .locals
        .iter()
        .filter_map(stack_entry_from_planned)
        .collect();
    let total_pulls = entries.len();
    if total_pulls > 1 {
        for p in &planned.locals {
            let Some(pull) = p.change.pull.as_ref() else {
                continue;
            };
            if pull.get("merged_at").is_some_and(|v| !v.is_null()) {
                continue;
            }
            let Some(number) = pull.get("number").and_then(Value::as_u64) else {
                continue;
            };
            comment_upsert::update_stack_comment_for_pull(
                opts.client,
                opts.user,
                opts.repo,
                number,
                &entries,
                &dest_branch,
                total_pulls,
            )
            .await?;
        }
    }
    log_lines.push("Comments updated.".into());

    // Revision-history comments — only for Updates, and only
    // when revision_history is enabled.
    if opts.revision_history {
        let now = Utc::now();
        for p in &planned.locals {
            if !matches!(p.change.action, Action::Update) {
                continue;
            }
            let Some(pull) = p.change.pull.as_ref() else {
                continue;
            };
            let Some(number) = pull.get("number").and_then(Value::as_u64) else {
                continue;
            };
            let Some(old_sha) = pull.pointer("/head/sha").and_then(Value::as_str) else {
                continue;
            };

            let ct = change_types
                .get(&p.change.change_id)
                .copied()
                .unwrap_or(ChangeType::Unknown);

            // Replay only when the change is content (not rebase
            // or unknown) — for rebase/unknown the
            // revision-history table renders without a compare
            // URL anyway.
            let replay_sha = if matches!(ct, ChangeType::Content) {
                replay::replay_for_revision(
                    opts.client,
                    Some(&repo_dir),
                    opts.user,
                    opts.repo,
                    old_sha,
                    &p.change.commit_sha,
                )
                .await
            } else {
                None
            };

            let input = RevisionInput {
                pull_number: number,
                old_sha,
                new_sha: &p.change.commit_sha,
                change_type: ct,
                reason: &p.change.note,
                replay_sha: replay_sha.as_deref(),
                timestamp: now,
            };
            comment_upsert::update_revision_history_for_pull(
                opts.client,
                opts.user,
                opts.repo,
                opts.github_server,
                &input,
            )
            .await?;
        }
        log_lines.push("Revision history updated.".into());
    }

    // Orphan-branch teardown — last so we don't yank the rug out
    // from anything earlier in the flow that might still reference
    // an orphan PR.
    for orphan in &planned.orphans {
        let Some(head_ref) = orphan.pointer("/head/ref").and_then(Value::as_str) else {
            continue;
        };
        pr_upsert::delete_orphan_branch(opts.client, opts.user, opts.repo, head_ref).await?;
    }

    log_lines.push("Finished.".into());

    Ok(Outcome::Pushed {
        planned,
        rebase: rebase_decision,
        upserted,
        log_lines,
    })
}

/// Build a `StackEntry` from a `PlannedChange` — pulls every
/// field the comment renderer needs out of the (possibly newly-
/// upserted) PR payload. Returns `None` for entries without a
/// pull (i.e. Create whose POST hasn't run, or a Skip-* without
/// any pull).
fn stack_entry_from_planned(p: &PlannedChange) -> Option<StackEntry> {
    let pull = p.change.pull.as_ref()?;
    let number = pull.get("number").and_then(Value::as_u64)?;
    let head_sha = pull
        .pointer("/head/sha")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let title = pull
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or(&p.change.title)
        .to_string();
    let html_url = pull
        .get("html_url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Some(StackEntry {
        number,
        change_id: p.change.change_id.clone(),
        head_sha,
        base_branch: p.base_branch.clone(),
        dest_branch: p.dest_branch.clone(),
        title,
        html_url,
    })
}

/// `decide_rebase` needs a [`crate::changes::Changes`] — bridge
/// from `PlannedChanges` by stripping the planner-added fields.
struct PlannedAsChanges<'a>(&'a PlannedChanges);

impl<'a> From<PlannedAsChanges<'a>> for crate::changes::Changes {
    fn from(p: PlannedAsChanges<'a>) -> Self {
        crate::changes::Changes {
            locals: p.0.locals.iter().map(|e| e.change.clone()).collect(),
            orphans: p.0.orphans.clone(),
        }
    }
}

fn compute_base_commit_sha(
    repo_dir: &Path,
    trunk_ref: &str,
    dest_branch: &str,
) -> Result<String, CliError> {
    // `--fork-point` is the precise answer when the reflog has
    // history; falls back to plain merge-base for fresh clones /
    // CI sandboxes. Same tolerance as `commands::sync`.
    if let Ok(sha) = run_git_capture(Some(repo_dir), &["merge-base", "--fork-point", trunk_ref]) {
        if !sha.is_empty() {
            return Ok(sha);
        }
    }
    let sha = run_git_capture(Some(repo_dir), &["merge-base", trunk_ref, "HEAD"])?;
    if sha.is_empty() {
        return Err(CliError::StackNotFound(format!(
            "common commit between `{trunk_ref}` and `{dest_branch}` branches not found",
        )));
    }
    Ok(sha)
}

fn git_count_behind(repo_dir: &Path, trunk_ref: &str) -> Result<u32, CliError> {
    let out = run_git_capture(
        Some(repo_dir),
        &["rev-list", "--count", &format!("HEAD..{trunk_ref}")],
    )?;
    out.parse::<u32>().map_err(|e| {
        CliError::Generic(format!(
            "`git rev-list --count` returned non-integer {out:?}: {e}"
        ))
    })
}

fn resolve_repo_toplevel(repo_dir: Option<&Path>) -> Result<std::path::PathBuf, CliError> {
    let raw = run_git_capture(repo_dir, &["rev-parse", "--show-toplevel"])?;
    Ok(std::path::PathBuf::from(raw))
}

fn git_cmd(repo_dir: Option<&Path>) -> Command {
    let mut cmd = Command::new("git");
    if let Some(dir) = repo_dir {
        cmd.arg("-C").arg(dir);
    }
    cmd.env("LC_ALL", "C").env("LANG", "C").env("LANGUAGE", "C");
    cmd
}

fn run_git_capture(repo_dir: Option<&Path>, args: &[&str]) -> Result<String, CliError> {
    let output = git_cmd(repo_dir)
        .args(args)
        .output()
        .map_err(|e| CliError::Generic(format!("failed to spawn `git {}`: {e}", args.join(" "))))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(CliError::Generic(if stderr.is_empty() {
            format!("`git {}` failed", args.join(" "))
        } else {
            stderr
        }));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_git_silent(repo_dir: &Path, args: &[&str]) -> Result<(), CliError> {
    let status = git_cmd(Some(repo_dir))
        .args(args)
        .status()
        .map_err(|e| CliError::Generic(format!("failed to spawn `git {}`: {e}", args.join(" "))))?;
    if !status.success() {
        return Err(CliError::Generic(format!(
            "`git {}` exited {status}",
            args.join(" "),
        )));
    }
    Ok(())
}
