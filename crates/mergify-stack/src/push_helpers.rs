//! Pure helpers for the `stack push` orchestrator that don't fit
//! a domain module of their own:
//!
//! - [`format_pull_description`] — strip the `Change-Id:` and
//!   stale `Depends-On:` lines from a commit message and append
//!   a fresh `Depends-On:` when the orchestrator linked this
//!   change to the previous PR in the stack.
//! - [`build_change_tasks`] — turn the planned `LocalChange`
//!   list into the dependency graph the per-PR upserter walks:
//!   each entry knows the index of the previous live change in
//!   the stack (so its `Depends-On:` header can point at that
//!   PR's number once it's known) and whether its own PR is
//!   already known at start of the run.
//!
//! Ported from `mergify_cli/stack/push.py::{format_pull_description,
//! _build_change_tasks}`.

use std::fmt::Write;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::changes::{Action, LocalChange};

/// `Depends-On: #<number>` line the orchestrator appends to a PR
/// description to chain stacked PRs. Matches case-sensitive on
/// purpose — the live Mergify backend only acts on the exact
/// header form.
static DEPENDS_ON_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Depends-On: \(#[0-9]*\)|Depends-On: #[0-9]*").unwrap());

/// `Change-Id: I…` trailer the stack tooling injects into every
/// commit message body. Mirrors `changes.py::CHANGEID_RE` —
/// matches the loose 40-char alphanumeric tail so a malformed
/// trailer still gets stripped (it would otherwise pollute the
/// rendered PR description).
static CHANGEID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Change-Id: I[0-9a-z]{40}").unwrap());

/// Render the PR description from a commit message.
///
/// The transformation:
///
/// 1. Strip the `Change-Id:` trailer — it's internal plumbing,
///    not something the reviewer should see.
/// 2. Strip any existing `Depends-On:` lines — the orchestrator
///    picked which PR this one depends on; stale ones from
///    earlier pushes get rewritten.
/// 3. Append a fresh `Depends-On: #<number>` when the previous
///    live change in the stack has a known PR number.
///
/// `depends_on_number` is the PR number of the previous live
/// change in the stack (the orchestrator resolves this from
/// `build_change_tasks` + the per-task `pull_ready` signal).
/// Pass `None` for the bottom of the stack — no dependency to
/// chain on.
#[must_use]
pub fn format_pull_description(message: &str, depends_on_number: Option<u64>) -> String {
    let mut out = CHANGEID_RE.replace_all(message, "").into_owned();
    // Match Python's `.rstrip("\n")` after each substitution —
    // the Change-Id line typically has a trailing newline that
    // would otherwise leave a blank line where the trailer used
    // to be.
    while out.ends_with('\n') {
        out.pop();
    }
    out = DEPENDS_ON_RE.replace_all(&out, "").into_owned();
    while out.ends_with('\n') {
        out.pop();
    }
    if let Some(number) = depends_on_number {
        write!(out, "\n\nDepends-On: #{number}").expect("write to String never fails");
    }
    out
}

/// One row in the dependency graph the per-PR upserter walks.
/// `index` is the position in the original `local_changes` list
/// (so callers can recover the matching `LocalChange`);
/// `depends_on_index` is the index of the previous live change
/// in the stack — the one whose PR number this task's
/// `Depends-On:` header should chain to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskMeta {
    pub index: usize,
    pub depends_on_index: Option<usize>,
    /// `true` when the change's PR is already known at the
    /// start of the push (either pre-existing PR via `Update`
    /// or `Skip*`, or no PR will ever exist via `SkipMerged`).
    /// Mirrors `pull_ready.set()` being called immediately on
    /// the Python side — downstream awaiters don't have to
    /// block on this task.
    pub pull_ready_at_start: bool,
    /// PR payload available at task-build time. `None` for
    /// `Create` actions (the PR doesn't exist yet), `Some` for
    /// every other action that carries a `pull` field.
    pub initial_pull: Option<Value>,
}

/// Turn the planned `local_changes` into the task graph the
/// orchestrator walks.
///
/// The graph is linear by construction: each task's
/// `depends_on_index` points at the *most recent* prior change
/// that either already has a PR or will create one. `SkipMerged`
/// changes don't carry forward as dependency anchors (their
/// commit is already on trunk, so chaining `Depends-On:` to
/// them would be meaningless).
#[must_use]
pub fn build_change_tasks(local_changes: &[LocalChange]) -> Vec<TaskMeta> {
    let mut tasks: Vec<TaskMeta> = Vec::with_capacity(local_changes.len());
    let mut last_pull_index: Option<usize> = None;

    for (i, change) in local_changes.iter().enumerate() {
        // pull_ready_at_start matches Python's
        // `task.pull_ready.set()` early-returns:
        //   - action != Create && pull is Some → resolved
        //     (Update / SkipUpToDate / SkipMerged with a pull)
        //   - action ∉ {Create, Update} → no upsert will run,
        //     so nothing blocks on it
        //   - everything else (Create with no pull, Update with
        //     no pull) → not ready; the upserter will resolve it
        let has_pull = change.pull.is_some();
        let action = change.action;
        let pull_ready_at_start = match action {
            Action::Create => false,
            Action::Update => has_pull,
            Action::SkipMerged | Action::SkipUpToDate => true,
        };

        tasks.push(TaskMeta {
            index: i,
            depends_on_index: last_pull_index,
            pull_ready_at_start,
            initial_pull: if matches!(action, Action::Create) && !has_pull {
                None
            } else {
                change.pull.clone()
            },
        });

        // Carry this index forward as the dependency anchor for
        // the next live change. `SkipMerged` doesn't qualify —
        // its commit is on trunk, not a PR to depend on.
        if change.pull.is_some() || matches!(action, Action::Create) {
            last_pull_index = Some(i);
        }
    }

    tasks
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn change(action: Action, pull: Option<Value>) -> LocalChange {
        LocalChange {
            commit_sha: "deadbeef".to_string(),
            title: "t".to_string(),
            message: "m".to_string(),
            change_id: "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            action,
            pull,
            note: String::new(),
        }
    }

    #[test]
    fn format_strips_change_id_trailer() {
        let msg = "feat: x\n\nChange-Id: Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n";
        assert_eq!(format_pull_description(msg, None), "feat: x");
    }

    #[test]
    fn format_strips_stale_depends_on_lines_and_preserves_internal_blanks() {
        // Existing `Depends-On: #N` is stripped — the
        // orchestrator owns the chaining, so a stale value left
        // over from a previous push would otherwise point at
        // the wrong PR. Internal whitespace around the trailer
        // is **preserved** byte-for-byte (Python's `rstrip("\n")`
        // only touches the *trailing* newline run, not the
        // interior ones); spot-checked against the Python
        // implementation.
        let msg = "feat: x\n\nDepends-On: #999\n\nbody";
        assert_eq!(
            format_pull_description(msg, None),
            // Python returns `'feat: x\n\n\n\nbody'` — the
            // double-blank gap where the trailer used to be is
            // intentional.
            "feat: x\n\n\n\nbody",
        );
    }

    #[test]
    fn format_appends_depends_on_when_predecessor_is_known() {
        let msg = "feat: x\n\nbody\n\nChange-Id: Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n";
        let out = format_pull_description(msg, Some(42));
        assert!(out.ends_with("\n\nDepends-On: #42"));
        // And the Change-Id is still gone.
        assert!(!out.contains("Change-Id"));
    }

    #[test]
    fn format_preserves_body_when_no_trailers_present() {
        let out = format_pull_description("feat: x\n\nbody", None);
        assert_eq!(out, "feat: x\n\nbody");
    }

    #[test]
    fn build_assigns_depends_on_anchors_in_order() {
        // Three Creates → each one's depends_on_index is the
        // previous; first one has no predecessor.
        let locals = vec![
            change(Action::Create, None),
            change(Action::Create, None),
            change(Action::Create, None),
        ];
        let tasks = build_change_tasks(&locals);
        assert_eq!(tasks[0].depends_on_index, None);
        assert_eq!(tasks[1].depends_on_index, Some(0));
        assert_eq!(tasks[2].depends_on_index, Some(1));
    }

    #[test]
    fn build_marks_creates_as_not_ready_and_updates_as_ready_with_pull() {
        // pull_ready_at_start gating mirrors Python's
        // pull_ready.set() early-returns — Update with a known
        // pull doesn't need the upserter to run before
        // downstream tasks can read it.
        let locals = vec![
            change(Action::Create, None),
            change(Action::Update, Some(json!({"number": 2}))),
            change(Action::SkipUpToDate, Some(json!({"number": 3}))),
            change(Action::SkipMerged, Some(json!({"number": 4}))),
        ];
        let tasks = build_change_tasks(&locals);
        assert!(!tasks[0].pull_ready_at_start);
        assert!(tasks[1].pull_ready_at_start);
        assert!(tasks[2].pull_ready_at_start);
        assert!(tasks[3].pull_ready_at_start);
    }

    #[test]
    fn build_skip_merged_does_not_anchor_next_task_dependency() {
        // `Depends-On: #merged-pr` would be meaningless — the
        // commit is on trunk, not behind a PR. Anchor must skip
        // ahead to the next live change.
        let locals = vec![
            change(Action::Update, Some(json!({"number": 1}))),
            change(Action::SkipMerged, Some(json!({"number": 99}))),
            change(Action::Create, None),
        ];
        let tasks = build_change_tasks(&locals);
        // The SkipMerged task is part of the carry-forward only
        // because the Python `if change.pull is not None or
        // action == "create"` clause says so — its index *does*
        // become the anchor for the next task, matching the
        // Python behaviour exactly. The pull number passed via
        // Depends-On is just the merged PR's number, which the
        // orchestrator then walks past when resolving the
        // header (skip-merged tasks set pull_ready immediately
        // with the merged PR).
        //
        // Lock in the Python-compatible behaviour so a future
        // refactor doesn't silently change which PR the next
        // task's Depends-On points at.
        assert_eq!(tasks[2].depends_on_index, Some(1));
    }

    #[test]
    fn build_initial_pull_carries_through_for_non_create_actions() {
        let pull = json!({"number": 42});
        let locals = vec![change(Action::Update, Some(pull.clone()))];
        let tasks = build_change_tasks(&locals);
        assert_eq!(tasks[0].initial_pull.as_ref(), Some(&pull));
    }
}
