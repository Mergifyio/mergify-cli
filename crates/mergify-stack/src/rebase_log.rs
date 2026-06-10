//! Render the log lines `stack push` emits to explain its
//! rebase / no-rebase choice.
//!
//! The body of every line carries the same information surfaced
//! by [`crate::approvals::RebaseDecision`] — *why* the orchestrator
//! decided to rebase or skip — phrased for a human reader. Three
//! entry points cover the three points in the push flow that
//! need to log:
//!
//! - [`rebase_performed`] — after a real rebase ran.
//! - [`rebase_skipped`] — after the orchestrator decided not
//!   to rebase. Returns `None` for the cases where there's
//!   nothing to log (no skip happened).
//! - [`rebase_dry_run`] — the `--dry-run` path; emits the
//!   would-have-rebased / would-have-skipped narration as a
//!   list of lines (the approvals-skip case fans out one row
//!   per approved PR).
//!
//! Pure formatters: they return strings instead of touching a
//! console so the caller decides how to print them and so the
//! tests don't have to mock a logger. Ported from
//! `mergify_cli/stack/push.py::{_log_rebase_performed,
//! _log_rebase_skipped, _log_rebase_dry_run}`.

use serde_json::Value;

use crate::approvals::{RebaseDecision, RebaseReason};

/// Log line for a completed rebase. `merged_count` is the number
/// of commits the sync step dropped (from
/// `sync_status.merged.len()` on the Python side); 0 elides the
/// `(dropped N merged commit(s))` clause entirely.
#[must_use]
pub fn rebase_performed(
    dest_branch: &str,
    remote: &str,
    base_branch: &str,
    merged_count: usize,
    decision: &RebaseDecision,
) -> String {
    let dropped = if merged_count > 0 {
        format!(" (dropped {merged_count} merged commit(s))")
    } else {
        String::new()
    };
    let prefix = format!("branch `{dest_branch}` rebased on `{remote}/{base_branch}`{dropped}");
    match decision.reason {
        RebaseReason::ConflictOverride => {
            let numbers = pull_numbers_csv(&decision.approved_pulls);
            format!(
                "{prefix} (bottom PR has conflicts; approvals on PR(s) {numbers} may be dismissed)",
            )
        }
        RebaseReason::Forced => format!("{prefix} (--force-rebase; approvals may be dismissed)"),
        _ => prefix,
    }
}

/// Log line for a skipped rebase. Returns `None` when the
/// decision doesn't represent a skip (i.e. the rebase actually
/// happened) — the orchestrator just doesn't log anything in
/// those cases.
#[must_use]
pub fn rebase_skipped(dest_branch: &str, decision: &RebaseDecision) -> Option<String> {
    match decision.reason {
        RebaseReason::ExplicitSkip => Some(format!(
            "branch `{dest_branch}` rebase skipped (--skip-rebase)"
        )),
        RebaseReason::SkippedForApprovals => {
            let n = decision.approved_pulls.len();
            let plural = if n == 1 { "" } else { "s" };
            let verb = if n == 1 { "has" } else { "have" };
            Some(format!(
                "branch `{dest_branch}` rebase skipped: {n} PR{plural} {verb} approvals \
                 (use --force-rebase to rebase anyway)",
            ))
        }
        // The rebase actually happened — nothing to log here;
        // `rebase_performed` handles those.
        _ => None,
    }
}

/// Multi-line narration for the `--dry-run` path. Returns the
/// log lines in the order they should be printed. Empty vec
/// means "nothing to say" — happens on `NoApprovals` when the
/// branch isn't behind, which is the no-op rebase case.
#[must_use]
pub fn rebase_dry_run(
    dest_branch: &str,
    remote: &str,
    base_branch: &str,
    commits_behind: u32,
    decision: &RebaseDecision,
) -> Vec<String> {
    match decision.reason {
        RebaseReason::ExplicitSkip => {
            vec![format!(
                "branch `{dest_branch}` rebase skipped (--skip-rebase)"
            )]
        }
        RebaseReason::SkippedForApprovals => {
            let n = decision.approved_pulls.len();
            let plural = if n == 1 { "" } else { "s" };
            let mut lines = vec![format!(
                "[orange]branch `{dest_branch}` rebase would be skipped: \
                 approvals detected on {n} PR{plural}[/]",
            )];
            for pull in &decision.approved_pulls {
                let number = pull.get("number").and_then(Value::as_u64).unwrap_or(0);
                let title = pull.get("title").and_then(Value::as_str).unwrap_or("");
                lines.push(format!("  - PR #{number} — \"{title}\""));
            }
            lines.push("  Use --force-rebase to rebase anyway.".to_string());
            lines
        }
        RebaseReason::ConflictOverride => {
            let numbers = pull_numbers_csv(&decision.approved_pulls);
            vec![format!(
                "[orange]branch `{dest_branch}` would be rebased on `{remote}/{base_branch}` \
                 (bottom PR has conflicts; approvals on PR(s) {numbers} would be dismissed)[/]",
            )]
        }
        RebaseReason::Forced => vec![format!(
            "[orange]branch `{dest_branch}` would be rebased on `{remote}/{base_branch}` \
             (--force-rebase; approvals may be dismissed)[/]",
        )],
        RebaseReason::NoApprovals => {
            // Match Python's "only warn if behind" — silent
            // no-op when the branch is already up-to-date.
            if commits_behind == 0 {
                return Vec::new();
            }
            let plural = if commits_behind == 1 {
                "commit"
            } else {
                "commits"
            };
            vec![format!(
                "[orange]branch `{dest_branch}` is behind `{remote}/{base_branch}` \
                 by {commits_behind} {plural}, commit SHAs will differ after rebase[/]",
            )]
        }
    }
}

fn pull_numbers_csv(pulls: &[Value]) -> String {
    pulls
        .iter()
        .filter_map(|p| p.get("number").and_then(Value::as_u64))
        .map(|n| format!("#{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decision(reason: RebaseReason, approved: Vec<Value>) -> RebaseDecision {
        RebaseDecision {
            should_rebase: matches!(
                reason,
                RebaseReason::Forced | RebaseReason::ConflictOverride | RebaseReason::NoApprovals,
            ),
            reason,
            approved_pulls: approved,
        }
    }

    #[test]
    fn performed_no_approvals_omits_qualifier() {
        let d = decision(RebaseReason::NoApprovals, Vec::new());
        assert_eq!(
            rebase_performed("feat/x", "origin", "main", 0, &d),
            "branch `feat/x` rebased on `origin/main`",
        );
    }

    #[test]
    fn performed_with_merged_drops_count_appears_in_prefix() {
        // The dropped-commits clause sits between the rebase
        // anchor and the optional reason — order matters because
        // Python pins this exact phrasing in the CLI output the
        // user reads.
        let d = decision(RebaseReason::NoApprovals, Vec::new());
        assert_eq!(
            rebase_performed("feat/x", "origin", "main", 2, &d),
            "branch `feat/x` rebased on `origin/main` (dropped 2 merged commit(s))",
        );
    }

    #[test]
    fn performed_conflict_override_lists_dismissed_approvals() {
        let d = decision(
            RebaseReason::ConflictOverride,
            vec![json!({"number": 12}), json!({"number": 34})],
        );
        let out = rebase_performed("feat/x", "origin", "main", 0, &d);
        assert!(out.contains("bottom PR has conflicts"));
        assert!(out.contains("#12, #34"));
    }

    #[test]
    fn performed_forced_mentions_force_rebase_flag() {
        let d = decision(RebaseReason::Forced, Vec::new());
        assert_eq!(
            rebase_performed("feat/x", "origin", "main", 0, &d),
            "branch `feat/x` rebased on `origin/main` (--force-rebase; approvals may be dismissed)",
        );
    }

    #[test]
    fn skipped_explicit_skip_line_matches_python() {
        let d = decision(RebaseReason::ExplicitSkip, Vec::new());
        assert_eq!(
            rebase_skipped("feat/x", &d),
            Some("branch `feat/x` rebase skipped (--skip-rebase)".to_string()),
        );
    }

    #[test]
    fn skipped_for_approvals_singular_grammar_for_one_pr() {
        let d = decision(
            RebaseReason::SkippedForApprovals,
            vec![json!({"number": 1})],
        );
        // 1 PR has approvals — singular verb + no plural marker.
        // Regress and a user sees "1 PRs have approvals" which
        // grates.
        assert_eq!(
            rebase_skipped("feat/x", &d).unwrap(),
            "branch `feat/x` rebase skipped: 1 PR has approvals \
             (use --force-rebase to rebase anyway)",
        );
    }

    #[test]
    fn skipped_for_approvals_plural_grammar_for_multiple_prs() {
        let d = decision(
            RebaseReason::SkippedForApprovals,
            vec![json!({"number": 1}), json!({"number": 2})],
        );
        assert_eq!(
            rebase_skipped("feat/x", &d).unwrap(),
            "branch `feat/x` rebase skipped: 2 PRs have approvals \
             (use --force-rebase to rebase anyway)",
        );
    }

    #[test]
    fn skipped_returns_none_for_non_skip_decisions() {
        // The rebase actually ran — there's nothing to "skip
        // line" log; the caller printed `rebase_performed`
        // instead.
        for r in [
            RebaseReason::Forced,
            RebaseReason::ConflictOverride,
            RebaseReason::NoApprovals,
        ] {
            assert!(rebase_skipped("feat/x", &decision(r, Vec::new())).is_none());
        }
    }

    #[test]
    fn dry_run_skipped_for_approvals_fans_out_one_line_per_pull() {
        // The orange header + per-PR rows + trailing
        // `Use --force-rebase` footer are the three pieces the
        // Python CLI prints; a regression would lose either the
        // titles or the trailing hint.
        let d = decision(
            RebaseReason::SkippedForApprovals,
            vec![
                json!({"number": 1, "title": "feat: a"}),
                json!({"number": 2, "title": "feat: b"}),
            ],
        );
        let lines = rebase_dry_run("feat/x", "origin", "main", 5, &d);
        assert_eq!(lines.len(), 4, "header + 2 rows + footer");
        assert!(lines[0].contains("approvals detected on 2 PRs"));
        assert_eq!(lines[1], "  - PR #1 — \"feat: a\"");
        assert_eq!(lines[2], "  - PR #2 — \"feat: b\"");
        assert_eq!(lines[3], "  Use --force-rebase to rebase anyway.");
    }

    #[test]
    fn dry_run_no_approvals_is_silent_when_branch_is_up_to_date() {
        // Match Python exactly — no orange "behind by 0 commits"
        // spam when the branch is current. A previous regression
        // emitted a line even at 0 commits behind.
        let d = decision(RebaseReason::NoApprovals, Vec::new());
        assert!(rebase_dry_run("feat/x", "origin", "main", 0, &d).is_empty());
    }

    #[test]
    fn dry_run_no_approvals_warns_when_branch_is_behind() {
        let d = decision(RebaseReason::NoApprovals, Vec::new());
        let lines = rebase_dry_run("feat/x", "origin", "main", 3, &d);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("behind `origin/main` by 3 commits"));
        // Singular grammar for `commits_behind == 1`.
        let lines = rebase_dry_run("feat/x", "origin", "main", 1, &d);
        assert!(lines[0].contains("behind `origin/main` by 1 commit"));
    }

    #[test]
    fn dry_run_forced_announces_intent_to_rebase() {
        let d = decision(RebaseReason::Forced, Vec::new());
        let lines = rebase_dry_run("feat/x", "origin", "main", 0, &d);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("would be rebased"));
        assert!(lines[0].contains("--force-rebase"));
    }
}
