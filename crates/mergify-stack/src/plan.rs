//! Full `stack push` planner — the layer above [`crate::changes::classify`]
//! that applies the orchestrator's CLI flags (`--next-only`,
//! `--only-update-existing-pulls`) and decorates each change with
//! the planning-time `dest_branch` / `base_branch` the upserter
//! needs.
//!
//! Ported from
//! `mergify_cli/stack/changes.py::get_changes`. The shape:
//!
//! 1. [`classify`] gives the per-commit base action
//!    (`Create`/`Update`/`SkipMerged`/`SkipUpToDate`).
//! 2. Apply [`PlannerOpts`] overrides:
//!    - `next_only && idx > 0` → action becomes [`SkipNextOnly`]
//!      so only the bottom of the stack actually pushes.
//!    - `only_update_existing_pulls && Create` → [`SkipCreate`]
//!      so the planner can surface "would-be-created" without
//!      opening a PR.
//! 3. Resolve `dest_branch`:
//!    - Existing pull → its `head.ref` (so the PR's branch
//!      survives renames).
//!    - No pull → `{stack_prefix}/{slug}` where `slug` comes
//!      from the Rust walker (Change-Id-stable across rewrites).
//! 4. Resolve `base_branch`: previous live change's `dest_branch`
//!    or `opts.base_branch` for the bottom of the stack.
//!
//! [`classify`]: crate::changes::classify
//! [`SkipNextOnly`]: crate::changes::Action::SkipNextOnly
//! [`SkipCreate`]: crate::changes::Action::SkipCreate

use mergify_core::CliError;
use serde::Serialize;
use serde_json::Value;

use crate::changes::{Action, LocalChange, classify};
use crate::local_commits::LocalCommit;
use crate::remote_changes::RemoteChange;

/// Flags from the `stack push` CLI that change which commits the
/// planner pushes.
#[derive(Debug, Clone, Copy)]
pub struct PlannerOpts<'a> {
    /// Branch prefix for synthesised PR branch names. Matches
    /// Python's `stack_prefix` arg — `stack/<user>` by default.
    pub stack_prefix: &'a str,
    /// Trunk branch the bottom of the stack targets. Matches
    /// Python's `base_branch` arg — typically `main`.
    pub base_branch: &'a str,
    /// `--only-update-existing-pulls`: a `Create` action gets
    /// overridden to `SkipCreate` so the run surfaces the
    /// would-be-created PR without actually opening one.
    pub only_update_existing_pulls: bool,
    /// `--next-only`: every change after the first gets
    /// `SkipNextOnly` so only the bottom of the stack lands.
    pub next_only: bool,
}

/// One planned change: the base [`LocalChange`] plus the
/// dest/base branches the upserter needs.
#[derive(Debug, Clone, Serialize)]
pub struct PlannedChange {
    #[serde(flatten)]
    pub change: LocalChange,
    /// Remote branch name the PR's head ref points at.
    pub dest_branch: String,
    /// Branch this PR targets (previous change's `dest_branch`
    /// for non-bottom rows; `opts.base_branch` for the bottom).
    pub base_branch: String,
}

/// Planner output: per-commit `PlannedChange`s plus the orphan
/// PR payloads (open PRs whose Change-Id is no longer in the
/// local stack).
#[derive(Debug, Clone, Serialize)]
pub struct PlannedChanges {
    pub locals: Vec<PlannedChange>,
    pub orphans: Vec<Value>,
}

/// Plan a `stack push` run.
///
/// Wraps [`classify`] with the orchestrator's overrides and the
/// dest/base branch resolution. Returns the same orphan list
/// [`classify`] does — pass-through, no override applies.
pub fn plan(
    local_commits: &[LocalCommit],
    remote_changes: Vec<RemoteChange>,
    opts: PlannerOpts<'_>,
) -> Result<PlannedChanges, CliError> {
    let classified = classify(local_commits, remote_changes)?;

    // Lookup commit_sha → slug so we can rebuild dest_branch
    // when a pull doesn't exist. The classifier dropped the
    // slug; pull it back from the walker output.
    let slug_by_sha: std::collections::HashMap<&str, &str> = local_commits
        .iter()
        .map(|c| (c.commit_sha.as_str(), c.slug.as_str()))
        .collect();

    let mut locals: Vec<PlannedChange> = Vec::with_capacity(classified.locals.len());

    for (idx, mut change) in classified.locals.into_iter().enumerate() {
        // CLI flag overrides. Order matters: --next-only first
        // (it short-circuits everything past the bottom of the
        // stack) so it doesn't fight with --only-update-existing-pulls.
        if opts.next_only && idx > 0 {
            change.action = Action::SkipNextOnly;
        } else if opts.only_update_existing_pulls && matches!(change.action, Action::Create) {
            change.action = Action::SkipCreate;
        }

        // dest_branch: existing pull's head.ref wins (so a
        // renamed PR branch survives), else synthesise from
        // the slug. Python matches.
        let dest_branch = change
            .pull
            .as_ref()
            .and_then(|p| p.pointer("/head/ref"))
            .and_then(Value::as_str)
            .map_or_else(
                || {
                    let slug = slug_by_sha
                        .get(change.commit_sha.as_str())
                        .copied()
                        .unwrap_or("");
                    format!("{}/{}", opts.stack_prefix, slug)
                },
                str::to_owned,
            );

        // base_branch: previous PlannedChange's dest_branch, or
        // the trunk for the bottom of the stack. Crucially the
        // *previous PlannedChange* — not the previous LocalChange
        // — so a SkipNextOnly chain still anchors on the
        // bottom's dest_branch.
        let base_branch = locals
            .last()
            .map_or_else(|| opts.base_branch.to_string(), |p| p.dest_branch.clone());

        locals.push(PlannedChange {
            change,
            dest_branch,
            base_branch,
        });
    }

    Ok(PlannedChanges {
        locals,
        orphans: classified.orphans,
    })
}

/// Convenience: rewrite every `SkipUpToDate` action to `Update`
/// after a rebase has been decided. Used by `stack push` when
/// the orchestrator chose to rebase but the planner had already
/// classified some commits as up-to-date — those PRs will get
/// new SHAs from the rebase, so they need to be updated too.
///
/// Mirrors the Python `planned_changes.replace_local_action(
/// old="skip-up-to-date", new="update")` post-decision tweak.
pub fn promote_skip_up_to_date_to_update(planned: &mut PlannedChanges) {
    for entry in &mut planned.locals {
        if matches!(entry.change.action, Action::SkipUpToDate) {
            entry.change.action = Action::Update;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn local(sha: &str, change_id: &str, title: &str) -> LocalCommit {
        LocalCommit {
            commit_sha: sha.to_string(),
            title: title.to_string(),
            message: String::new(),
            change_id: change_id.to_string(),
            slug: format!("{title}--00000000"),
            note: String::new(),
        }
    }

    fn pull_open(change_id: &str, head_sha: &str, head_ref: &str, number: u64) -> RemoteChange {
        RemoteChange {
            change_id: change_id.to_string(),
            pull: json!({
                "number": number,
                "state": "open",
                "merged_at": null,
                "head": {"sha": head_sha, "ref": head_ref},
            }),
        }
    }

    fn opts<'a>(stack_prefix: &'a str, base_branch: &'a str) -> PlannerOpts<'a> {
        PlannerOpts {
            stack_prefix,
            base_branch,
            only_update_existing_pulls: false,
            next_only: false,
        }
    }

    #[test]
    fn dest_branch_synthesised_from_slug_when_no_pull() {
        let cid = "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01";
        let planned = plan(
            &[local("abc", cid, "feat-a")],
            vec![],
            opts("stack/jd", "main"),
        )
        .unwrap();
        assert_eq!(planned.locals[0].dest_branch, "stack/jd/feat-a--00000000");
        assert_eq!(planned.locals[0].base_branch, "main");
    }

    #[test]
    fn dest_branch_uses_existing_pull_head_ref_when_present() {
        // A PR rename (e.g. user reverted a renamed dest_branch on
        // GitHub) must survive — the planner uses the live PR's
        // head.ref, not the locally-derived slug.
        let cid = "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01";
        let pull = pull_open(cid, "abc", "stack/jd/custom-name", 1);
        let planned = plan(
            &[local("abc", cid, "feat-a")],
            vec![pull],
            opts("stack/jd", "main"),
        )
        .unwrap();
        assert_eq!(planned.locals[0].dest_branch, "stack/jd/custom-name");
    }

    #[test]
    fn base_branch_chains_to_previous_dest_branch() {
        // Stack of two creates: first base = trunk, second base =
        // first's dest_branch. That's how PR-base chaining
        // produces the "stacked PR" review UX on GitHub.
        let cid1 = "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01";
        let cid2 = "Ibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb02";
        let planned = plan(
            &[local("abc", cid1, "feat-a"), local("def", cid2, "feat-b")],
            vec![],
            opts("stack/jd", "main"),
        )
        .unwrap();
        assert_eq!(planned.locals[0].base_branch, "main");
        assert_eq!(planned.locals[1].base_branch, planned.locals[0].dest_branch);
    }

    #[test]
    fn next_only_skips_every_change_past_the_bottom() {
        // bottom Create stays Create; everything above becomes
        // SkipNextOnly regardless of what classify would have
        // emitted (Update / SkipUpToDate / etc).
        let cid1 = "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01";
        let cid2 = "Ibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb02";
        let cid3 = "Icccccccccccccccccccccccccccccccccccccc03";
        let planned = plan(
            &[
                local("abc", cid1, "feat-a"),
                local("def", cid2, "feat-b"),
                local("ghi", cid3, "feat-c"),
            ],
            vec![pull_open(cid2, "def", "stack/jd/b", 2)],
            PlannerOpts {
                next_only: true,
                ..opts("stack/jd", "main")
            },
        )
        .unwrap();
        assert_eq!(planned.locals[0].change.action, Action::Create);
        assert_eq!(planned.locals[1].change.action, Action::SkipNextOnly);
        assert_eq!(planned.locals[2].change.action, Action::SkipNextOnly);
    }

    #[test]
    fn only_update_existing_pulls_turns_create_into_skip_create() {
        // `Create` → `SkipCreate` so the dry-run path can surface
        // "would-be-created" without opening a PR. Updates and
        // skips pass through unchanged.
        let cid1 = "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01";
        let cid2 = "Ibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb02";
        let planned = plan(
            &[local("abc", cid1, "feat-a"), local("def", cid2, "feat-b")],
            vec![pull_open(cid2, "xxx", "stack/jd/b", 2)],
            PlannerOpts {
                only_update_existing_pulls: true,
                ..opts("stack/jd", "main")
            },
        )
        .unwrap();
        assert_eq!(planned.locals[0].change.action, Action::SkipCreate);
        assert_eq!(planned.locals[1].change.action, Action::Update);
    }

    #[test]
    fn next_only_wins_over_only_update_existing_pulls_above_the_bottom() {
        // Both flags set, idx > 0 → `SkipNextOnly` wins over
        // `SkipCreate`. Order in the planner matters: a
        // regression would have made every `Create` above the
        // bottom show up as `SkipCreate` instead of
        // `SkipNextOnly`, which the dry-run surfaces differently.
        // (At idx == 0 only_update_existing_pulls applies and the
        // bottom becomes `SkipCreate` — that's Python's behaviour
        // too.)
        let cid1 = "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01";
        let cid2 = "Ibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb02";
        let planned = plan(
            &[local("abc", cid1, "feat-a"), local("def", cid2, "feat-b")],
            vec![],
            PlannerOpts {
                next_only: true,
                only_update_existing_pulls: true,
                ..opts("stack/jd", "main")
            },
        )
        .unwrap();
        assert_eq!(planned.locals[0].change.action, Action::SkipCreate);
        assert_eq!(planned.locals[1].change.action, Action::SkipNextOnly);
    }

    #[test]
    fn promote_rewrites_skip_up_to_date_to_update() {
        // Post-rebase-decision tweak: a `SkipUpToDate` PR's SHA
        // will change after the rebase, so the orchestrator
        // promotes it to `Update` to force the push.
        let cid = "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01";
        let mut planned = plan(
            &[local("abc", cid, "feat-a")],
            vec![pull_open(cid, "abc", "stack/jd/feat-a", 1)],
            opts("stack/jd", "main"),
        )
        .unwrap();
        assert_eq!(planned.locals[0].change.action, Action::SkipUpToDate);
        promote_skip_up_to_date_to_update(&mut planned);
        assert_eq!(planned.locals[0].change.action, Action::Update);
    }

    #[test]
    fn orphans_pass_through_from_classify() {
        // The planner doesn't override orphan behaviour — the
        // CLI's orphan-handling logic stays in the orchestrator.
        let cid = "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01";
        let planned = plan(
            &[],
            vec![pull_open(cid, "abc", "stack/jd/feat-x", 7)],
            opts("stack/jd", "main"),
        )
        .unwrap();
        assert_eq!(planned.orphans.len(), 1);
    }
}
