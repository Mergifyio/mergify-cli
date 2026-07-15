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
//! 7. Prepare each Update's revision history and write it as a
//!    git note on the new head commit (`crate::revision_note`),
//!    so step 8's atomic push carries it.
//! 8. `git push --atomic` via [`crate::notes_push`].
//! 9. Upsert each PR sequentially via [`crate::pr_upsert`] so
//!    each `Depends-On: #<n>` header sees the predecessor's
//!    freshly-created PR number.
//! 10. Upsert stack comments, and render + upsert each prepared
//!     revision-history comment, per PR via [`crate::comment_upsert`].
//! 11. Tear down orphan branches.
//!
//! PR upserts run sequentially. Async here is incidental — it comes
//! from reqwest's async-only client, not from a need for concurrency:
//! for the typical 2–5 PR stacks the latency is dominated by the
//! GitHub round-trip, and sequential code avoids a
//! `tokio::sync::Notify` dependency graph.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::git::{compute_base_commit_sha, resolve_repo_toplevel, run_git_capture, run_git_silent};

use chrono::Utc;
use mergify_core::{CliError, HttpClient};
use serde_json::Value;

use crate::approvals::{self, RebaseDecision};
use crate::change_type::{self, ChangeType};
use crate::changes::Action;
use crate::commands::sync as sync_cmd;
use crate::comment_upsert;
use crate::local_commits;
use crate::notes_push::{self, NotesLease, PushEntry};
use crate::plan::{self, PlannedChange, PlannedChanges, PlannerOpts};
use crate::pr_upsert::{self, PrUpsertInput, StaleBase};
use crate::progress::{Mark, Progress};
use crate::rebase_log;
use crate::remote_changes;
use crate::replay;
use crate::revision_history::RevisionHistoryComment;
use crate::revision_note;
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
/// decision and the buffered `log_lines` the CLI's dry-run path
/// prints. `Pushed` carries the final plan + rebase decision; the
/// real-push path streams its per-PR results live (see
/// [`crate::progress`]) rather than buffering them, so there is
/// nothing further to return.
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
        return Err(stack_context::targets_itself_error(
            Some(&repo_dir),
            &dest_branch,
            remote,
            base_branch,
        ));
    }

    let stack_prefix = if opts.branch_prefix.is_empty() {
        dest_branch.clone()
    } else {
        format!("{prefix}/{dest_branch}", prefix = opts.branch_prefix)
    };

    // The pre-flight — trunk + notes fetch, the PR search, the rebase
    // decision — is several network round-trips with nothing to show,
    // the stretch that used to look frozen. Drive one spinner row
    // through it, resolving to a kept summary line so it never blinks
    // out, then seal so the live block (or the dry-run plan) starts
    // fresh below.
    let mut prog = Progress::new();
    let pf = prog.add("fetching trunk");

    // The trunk + notes fetch is synchronous git; run it on a
    // blocking thread so the spinner keeps ticking smoothly across it.
    let notes_lease = {
        let repo = repo_dir.clone();
        let remote = remote.to_string();
        let base = base_branch.to_string();
        prog.run(
            pf,
            "fetching trunk",
            spawn_git_blocking("git fetch", move || -> Result<NotesLease, CliError> {
                run_git_silent(Some(&repo), &["fetch", &remote, &base])?;
                notes_push::fetch_notes_ref(Some(&repo), &remote)
            }),
        )
        .await?
    };

    let trunk_ref = format!("{remote}/{base_branch}");
    let base_commit_sha = compute_base_commit_sha(&repo_dir, &trunk_ref, &dest_branch)?;

    // `get_remote_changes` fetches each PR sequentially. Publish the
    // number it's on into a shared cell; the spinner's tick loop reads
    // it each frame so the label tracks each pull (owner/repo#id)
    // while the glyph keeps spinning smoothly.
    let reading = Arc::new(AtomicU64::new(0));
    let reading_writer = Arc::clone(&reading);
    let fetch_pulls = remote_changes::get_remote_changes_reporting(
        opts.client,
        opts.user,
        opts.repo,
        &stack_prefix,
        opts.author,
        move |number| reading_writer.store(number, Ordering::Relaxed),
    );
    let (owner, repo_name) = (opts.user, opts.repo);
    let remote_changes_data = prog
        .run_reporting(pf, "reading pull requests", fetch_pulls, move || {
            let n = reading.load(Ordering::Relaxed);
            (n != 0).then(|| format!("reading {owner}/{repo_name}#{n}"))
        })
        .await?;
    let local = local_commits::read(&repo_dir, &base_commit_sha, "HEAD")?;

    let planner_opts = PlannerOpts {
        stack_prefix: &stack_prefix,
        base_branch,
        only_update_existing_pulls: opts.only_update_existing_pulls,
        next_only: opts.next_only,
    };
    let mut planned = plan::plan(&local, remote_changes_data.clone(), planner_opts)?;

    let rebase_decision = prog
        .run(
            pf,
            "checking rebase",
            approvals::decide_rebase(
                opts.client,
                opts.user,
                opts.repo,
                &PlannedAsChanges(&planned).into(),
                opts.skip_rebase,
                opts.force_rebase,
            ),
        )
        .await?;
    // Resolve the pre-flight spinner to a kept summary line (not
    // erased — no blank gap) and seal it so the rebase's own git
    // output, and the live blocks below, start fresh underneath.
    let pre_flight_summary = format!("read {n} pull request(s)", n = remote_changes_data.len());
    prog.resolve(pf, Mark::Done, Some(&pre_flight_summary));
    prog.seal();

    if opts.dry_run {
        let mut log_lines: Vec<String> = vec!["Stacked pull request plan:".into()];
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
        push_plan_preview(&mut log_lines, &planned, opts.create_as_draft);
        log_lines.push("Finished (dry-run mode).".into());
        return Ok(Outcome::DryRun {
            planned,
            rebase: rebase_decision,
            commits_behind,
            log_lines,
        });
    }

    // Real-push path. `prog` (created above for the pre-flight) now
    // drives the live per-PR rows. Live streaming replaces the old
    // buffered plan-then-result transcript so a slow link never looks
    // frozen and PRs aren't listed twice; off a TTY it degrades to
    // one plain line per step (see [`crate::progress`]).
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
            quiet: true,
            // The pre-flight already fetched the trunk and the remote
            // PRs; hand them through so the rebase doesn't repeat the
            // search + per-PR GETs.
            prefetched_remote_changes: Some(remote_changes_data.clone()),
            skip_trunk_fetch: true,
        };
        // Drive the rebase under a spinner — it re-reads the remote
        // PRs, fetches trunk, and rebases, which used to be a silent
        // stall. `quiet` captures git's output so it can't corrupt
        // the in-place redraw.
        let ridx = prog.add("rebasing");
        let outcome = prog
            .run(
                ridx,
                format!("rebasing onto `{remote}/{base_branch}`"),
                sync_cmd::run(&sync_opts),
            )
            .await?;
        let dropped = match outcome {
            sync_cmd::Outcome::Synced { dropped_count, .. } => dropped_count,
            sync_cmd::Outcome::DryRun(_) => 0,
        };
        prog.resolve(
            ridx,
            Mark::Done,
            Some(&rebase_log::rebase_performed(
                &dest_branch,
                remote,
                base_branch,
                dropped,
                &rebase_decision,
            )),
        );

        // Rebase changed local SHAs — recompute base + re-plan.
        let new_base = compute_base_commit_sha(&repo_dir, &trunk_ref, &dest_branch)?;
        let local2 = local_commits::read(&repo_dir, &new_base, "HEAD")?;
        planned = plan::plan(&local2, remote_changes_data, planner_opts)?;
    } else if let Some(line) = rebase_log::rebase_skipped(&dest_branch, &rebase_decision) {
        prog.add_resolved(Mark::Noop, line);
    }

    // Publishing spinner — covers change-type detection, base
    // neutralization, and the push, so there's no silent gap between
    // the rebase note and "pushed". Only when something is created or
    // updated; a fully up-to-date stack has none of these.
    let publishing = planned
        .locals
        .iter()
        .any(|p| matches!(p.change.action, Action::Create | Action::Update))
        .then(|| prog.add("preparing"));

    // Pre-push change-type detection for the revision-history comment,
    // before `push_branches` overwrites the remote refs. The old-PR-
    // head fetch is network git, so run it on a blocking thread under
    // the spinner. A failure only downgrades change types to
    // 'unknown', so defer the warning past the live block rather than
    // corrupt it with a mid-block print.
    let mut change_types: std::collections::HashMap<String, ChangeType> =
        std::collections::HashMap::new();
    let mut deferred_notes: Vec<String> = Vec::new();
    let mut revision_histories: std::collections::HashMap<String, (u64, RevisionHistoryComment)> =
        std::collections::HashMap::new();
    if opts.revision_history {
        let updated_pr_numbers: Vec<u64> = planned
            .locals
            .iter()
            .filter(|p| matches!(p.change.action, Action::Update))
            .filter_map(|p| pull_number(p.change.pull.as_ref()))
            .collect();
        if !updated_pr_numbers.is_empty() {
            let repo = repo_dir.clone();
            let remote = remote.to_string();
            let numbers = updated_pr_numbers;
            let fetch = spawn_git_blocking("change-type fetch", move || {
                change_type::fetch_old_pr_heads(Some(&repo), &remote, &numbers)
            });
            let fetched = prog
                .run_optional(publishing, "analyzing changes", fetch)
                .await;
            if let Err(e) = fetched {
                deferred_notes.push(format!(
                    "Could not fetch old PR heads; revision-history \
                     change types will fall back to 'unknown': {e}",
                ));
            }
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

        // Build each Update's full revision history BEFORE the push:
        // the history is written as a git note on the new head commit
        // so the atomic push below carries it with the branches. The
        // Mergify engine copies that note onto the merge commit at
        // merge time, so it must be on the remote before the PR can
        // merge — writing it after the push would leave a window.
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

            // A marker-carrying note on the local commit is a history
            // note left by a previous push attempt that failed before
            // the branch landed. If it records exactly this old→new
            // transition, reuse it verbatim — rebuilding from the old
            // head's note would lose the reason the user attached
            // before the failed attempt — and skip the replay/change-
            // type work below entirely: the recovered entry already
            // carries its own replay_sha, and replay uploads objects
            // to GitHub, so it must not run pointlessly.
            if crate::revision_history::contains_marker(&p.change.note)
                && let Some(history) = revision_note::recover_pending(
                    &p.change.note,
                    opts.github_server,
                    opts.user,
                    opts.repo,
                    old_sha,
                    &p.change.commit_sha,
                )
            {
                revision_note::write_note(
                    Some(&repo_dir),
                    &p.change.commit_sha,
                    &revision_note::render(&history, number),
                )?;
                revision_histories.insert(p.change.change_id.clone(), (number, history));
                continue;
            }

            let ct = change_types
                .get(&p.change.change_id)
                .copied()
                .unwrap_or(ChangeType::Unknown);

            // Replay only when the change is content (not rebase or
            // unknown) — for rebase/unknown the revision-history
            // table renders without a compare URL anyway.
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

            // A marker-carrying note on the local commit that didn't
            // match the recovery check above is stale machine history
            // (another machine pushed meanwhile, or a different
            // amend followed the failed attempt) — not a user reason.
            let reason = if crate::revision_history::contains_marker(&p.change.note) {
                ""
            } else {
                p.change.note.as_str()
            };

            let history = match revision_note::load_or_seed(
                opts.client,
                Some(&repo_dir),
                opts.github_server,
                opts.user,
                opts.repo,
                number,
                old_sha,
            )
            .await
            {
                Ok(Some(mut h)) => {
                    h.append(
                        old_sha,
                        &p.change.commit_sha,
                        ct,
                        now,
                        reason,
                        replay_sha.as_deref(),
                    );
                    h
                }
                Ok(None) => RevisionHistoryComment::create_initial(
                    opts.github_server,
                    opts.user,
                    opts.repo,
                    old_sha,
                    &p.change.commit_sha,
                    ct,
                    now,
                    reason,
                    replay_sha.as_deref(),
                ),
                Err(e) => {
                    // A transient failure (e.g. rate limit) fetching
                    // the migration-seed PR comment must not fall
                    // back to `create_initial`: that would write a
                    // fresh 2-entry history and, worse, PATCH over
                    // the PR comment that was the only remaining
                    // seed — permanently destroying history a retry
                    // could otherwise have recovered. Leave this PR's
                    // history untouched this push (no note write, no
                    // comment update): it converges on the next push
                    // once the GET succeeds, since neither the old
                    // head's note nor the comment changed meanwhile.
                    deferred_notes.push(format!(
                        "Could not load previous revision history for #{number}; \
                         leaving it untouched this push, will retry next push: {e}",
                    ));
                    continue;
                }
            };

            revision_note::write_note(
                Some(&repo_dir),
                &p.change.commit_sha,
                &revision_note::render(&history, number),
            )?;
            revision_histories.insert(p.change.change_id.clone(), (number, history));
        }
    }

    // Before the force-push, repoint any Update PR whose base is
    // moving onto the trunk. A reorder can make a head branch
    // briefly an ancestor of its stale base once the atomic push
    // lands, which GitHub auto-closes as "merged" (see
    // `pr_upsert::neutralize_stale_bases` docs for the full
    // rationale).
    let stale_bases: Vec<StaleBase<'_>> = planned
        .locals
        .iter()
        .filter(|p| matches!(p.change.action, Action::Update))
        .filter_map(|p| {
            let pull = p.change.pull.as_ref()?;
            let number = pull.get("number").and_then(Value::as_u64)?;
            let current_base_ref = pull.pointer("/base/ref").and_then(Value::as_str)?;
            Some(StaleBase {
                pull_number: number,
                current_base_ref,
                new_base_ref: &p.base_branch,
            })
        })
        .collect();
    let neutralize = pr_upsert::neutralize_stale_bases(
        opts.client,
        opts.user,
        opts.repo,
        &stale_bases,
        base_branch,
    );
    prog.run_optional(publishing, "preparing branches", neutralize)
        .await?;

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
    let pushed = push_entries.len();
    {
        // Synchronous git push on a blocking thread so the spinner
        // keeps ticking across the network round-trip.
        let repo = repo_dir.clone();
        let remote_owned = remote.to_string();
        let no_verify = opts.no_verify;
        let push_notes_lease = notes_lease.clone();
        let push = spawn_git_blocking("git push", move || {
            notes_push::push_branches(
                Some(&repo),
                &remote_owned,
                &push_entries,
                no_verify,
                &push_notes_lease,
            )
        });
        prog.run_optional(publishing, format!("pushing {pushed} branch(es)"), push)
            .await?;
    }
    if pushed > 0 && matches!(notes_lease, NotesLease::Unknown) {
        deferred_notes.push(
            "Could not determine the remote notes state; stack notes were not \
             pushed and will catch up on the next push."
                .to_string(),
        );
    }
    if let Some(idx) = publishing {
        prog.resolve(
            idx,
            Mark::Done,
            Some(&format!("pushed {pushed} branch(es)")),
        );
    }

    // One live row per planned change, in stack order. Create/Update
    // start `queued` and spin while their upsert is in flight; the
    // skip variants resolve immediately. `row_idx[i]` maps each local
    // back to its row for the upsert loop below.
    let mut row_idx: Vec<Option<usize>> = Vec::with_capacity(planned.locals.len());
    for p in &planned.locals {
        let url = pull_url(p.change.pull.as_ref());
        let title = p.change.title.clone();
        match p.change.action {
            Action::Create | Action::Update => {
                // Dim SHA transition shown beside the status: the old
                // PR-head short SHA → the new commit short SHA for an
                // update, just the new short SHA for a create.
                let new7 = short_sha(&p.change.commit_sha);
                let detail = match p.change.action {
                    Action::Update => pull_head_short_sha(p.change.pull.as_ref())
                        .map(|old7| format!("{old7}→{new7}")),
                    _ => Some(new7),
                };
                row_idx.push(Some(prog.add_pr(title, "queued", detail, None)));
            }
            Action::SkipMerged => {
                prog.add_resolved_pr(title, Mark::Merged, "merged", url);
                row_idx.push(None);
            }
            Action::SkipUpToDate => {
                prog.add_resolved_pr(title, Mark::Noop, "up-to-date", url);
                row_idx.push(None);
            }
            Action::SkipCreate | Action::SkipNextOnly => {
                prog.add_resolved_pr(title, Mark::Noop, "skipped", url);
                row_idx.push(None);
            }
        }
    }

    // Sequential per-PR upsert so each Depends-On has access to the
    // predecessor's freshly-known PR number — and fine for typical
    // stack sizes (see the module doc on why this isn't parallelised).
    let mut last_pull_number: Option<u64> = None;
    for (i, entry) in planned.locals.iter_mut().enumerate() {
        let action = entry.change.action;
        if !matches!(action, Action::Create | Action::Update) {
            // Skip-* still carries an existing pull number
            // forward as a potential predecessor for downstream
            // Depends-On chaining. Mirrors Python's
            // `_build_change_tasks` carry-forward semantics.
            if let Some(n) = pull_number(entry.change.pull.as_ref()) {
                last_pull_number = Some(n);
            }
            continue;
        }
        let idx = row_idx[i].expect("create/update rows are always added");

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
        let active = if matches!(action, Action::Create) {
            "creating"
        } else {
            "updating"
        };
        let pull = prog
            .run(
                idx,
                active,
                pr_upsert::create_or_update_pr(opts.client, opts.user, opts.repo, input),
            )
            .await?;
        let number = pull.get("number").and_then(Value::as_u64);
        if let Some(n) = number {
            last_pull_number = Some(n);
        }
        let url = pull
            .get("html_url")
            .and_then(Value::as_str)
            .map(str::to_string);
        prog.set_url(idx, url);
        let done_word = if matches!(action, Action::Create) {
            "created"
        } else {
            "updated"
        };
        prog.resolve(idx, Mark::Done, Some(done_word));
        entry.change.pull = Some(pull);
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
        let cidx = prog.add("queued");
        prog.run(cidx, "updating stack comments", async {
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
            Ok::<(), CliError>(())
        })
        .await?;
        prog.resolve(cidx, Mark::Done, Some("stack comments updated"));
    }

    // Revision-history comments — rendered from the histories
    // prepared (and written as git notes) before the push.
    if !revision_histories.is_empty() {
        let ridx = prog.add("queued");
        prog.run(ridx, "updating revision history", async {
            for p in &planned.locals {
                let Some((number, history)) = revision_histories.get(&p.change.change_id) else {
                    continue;
                };
                comment_upsert::upsert_revision_history_comment(
                    opts.client,
                    opts.user,
                    opts.repo,
                    *number,
                    &history.body(*number),
                )
                .await?;
            }
            Ok::<(), CliError>(())
        })
        .await?;
        prog.resolve(ridx, Mark::Done, Some("revision history updated"));
    }

    // Orphan-branch teardown — last so we don't yank the rug out
    // from anything earlier in the flow that might still reference
    // an orphan PR.
    for orphan in &planned.orphans {
        let Some(head_ref) = orphan.pointer("/head/ref").and_then(Value::as_str) else {
            continue;
        };
        let title = orphan
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>")
            .to_string();
        let url = orphan
            .get("html_url")
            .and_then(Value::as_str)
            .map(str::to_string);
        let oidx = prog.add_pr(title, "queued", None, None);
        prog.set_url(oidx, url);
        prog.run(
            oidx,
            "deleting",
            pr_upsert::delete_orphan_branch(opts.client, opts.user, opts.repo, head_ref),
        )
        .await?;
        prog.resolve(oidx, Mark::Noop, Some("deleted"));
    }

    // Warnings stashed during the live block (a mid-block print would
    // corrupt the in-place redraw) surface now, after the last row.
    for note in deferred_notes {
        prog.note(note);
    }
    // Real-push streams live; the buffered transcript is unused.
    // When nothing reached the remote — no branch pushed, no orphan
    // deleted — the stack already matched GitHub. A preceding rebase
    // only rewrote local refs, so it still reports as up to date.
    let closing = if pushed == 0 && planned.orphans.is_empty() {
        "Already up to date."
    } else {
        "Finished."
    };
    let _ = prog.finish(closing);

    Ok(Outcome::Pushed {
        planned,
        rebase: rebase_decision,
    })
}

/// Append the "what will happen" preview lines (locals first, then
/// orphans), in the would-be wording. Mirrors Python's
/// `changes.display_plan`.
fn push_plan_preview(log: &mut Vec<String>, planned: &PlannedChanges, create_as_draft: bool) {
    for p in &planned.locals {
        log.push(crate::changes::format_local_change_log(
            &p.change,
            &p.dest_branch,
            true,
            create_as_draft,
        ));
    }
    for orphan in &planned.orphans {
        log.push(crate::changes::format_orphan_change_log(orphan, true));
    }
}

/// PR number from a pull payload, if present.
fn pull_number(pull: Option<&Value>) -> Option<u64> {
    pull?.get("number").and_then(Value::as_u64)
}

/// First 7 chars of a SHA — the conventional short form shown in
/// the progress detail.
fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// Short head SHA of a PR payload, if present.
fn pull_head_short_sha(pull: Option<&Value>) -> Option<String> {
    pull?
        .pointer("/head/sha")
        .and_then(Value::as_str)
        .map(short_sha)
}

/// Run a synchronous git operation on a blocking thread, mapping a
/// task-join panic into a `CliError`. Collapses the
/// `spawn_blocking(...).await.map_err(...)?` boilerplate the push
/// flow repeats for each off-executor git step (`label` names the
/// step in the panic message).
async fn spawn_git_blocking<T, F>(label: &str, f: F) -> Result<T, CliError>
where
    F: FnOnce() -> Result<T, CliError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| CliError::Generic(format!("{label} task panicked: {e}")))?
}

/// PR `html_url` from a pull payload, if present.
fn pull_url(pull: Option<&Value>) -> Option<String> {
    pull?
        .get("html_url")
        .and_then(Value::as_str)
        .map(str::to_string)
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
