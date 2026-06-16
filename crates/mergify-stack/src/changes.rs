//! Full stack-change classifier.
//!
//! Walks the local stack commits, matches each to a remote PR by
//! `Change-Id`, and produces a `LocalChange` per commit tagged
//! with the same `Action` enum the Python `get_changes` returns.
//! `stack list`, `stack open`, and (eventually) `stack push` all
//! consume this; the merged-vs-remaining subset that `stack sync`
//! uses lives in [`crate::sync_status`] for the same reason —
//! sync only needs the bucket, not the full classifier.

use mergify_core::CliError;
use serde::Serialize;
use serde_json::Value;

use crate::local_commits::LocalCommit;
use crate::remote_changes::RemoteChange;

/// Action attached to each local commit. Matches Python's
/// `ActionT` literal union, including the two orchestrator-only
/// overrides (`SkipCreate`, `SkipNextOnly`) that [`classify`]
/// never produces but [`plan`] can.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// Commit was merged via its PR (head SHA matches). The PR's
    /// commit is already in trunk; future syncs drop it.
    SkipMerged,
    /// PR head SHA matches the local commit — nothing to push.
    SkipUpToDate,
    /// Commit has no PR yet — would be created on the next push.
    Create,
    /// Commit has an open PR whose head differs — would be updated.
    Update,
    /// `--only-update-existing-pulls` flag turns `Create` into
    /// `SkipCreate` so the planner can surface "would-be-created"
    /// PRs without actually opening them.
    SkipCreate,
    /// `--next-only` flag turns every change after the first into
    /// `SkipNextOnly` so only the bottom of the stack gets pushed.
    /// Pull is preserved when one already existed — the orchestrator
    /// just doesn't touch it this run.
    SkipNextOnly,
}

/// One commit's classification result.
#[derive(Debug, Clone, Serialize)]
pub struct LocalChange {
    pub commit_sha: String,
    pub title: String,
    pub message: String,
    pub change_id: String,
    pub action: Action,
    /// Matched PR payload from GitHub when one exists. Carried
    /// through as `serde_json::Value` so renderers can read any
    /// PR field without forcing a typed schema.
    pub pull: Option<Value>,
    pub note: String,
}

/// Output of the classifier. `orphans` are remote changes that
/// don't match any local commit (open PRs whose Change-Id isn't
/// in the local stack — typically the user dropped a commit
/// locally without closing the PR).
#[derive(Debug, Clone, Serialize)]
pub struct Changes {
    pub locals: Vec<LocalChange>,
    pub orphans: Vec<Value>,
}

/// Classify every local commit. `remote_changes` is consumed —
/// each matched pull is removed so we can identify orphans at
/// the end.
pub fn classify(
    local_commits: &[LocalCommit],
    mut remote_changes: Vec<RemoteChange>,
) -> Result<Changes, CliError> {
    let mut locals = Vec::with_capacity(local_commits.len());

    for local in local_commits {
        let pull = pop_matching(&mut remote_changes, &local.change_id);

        // Pre-emptively peek the merge state — it affects the
        // action choice in two places.
        let merged = pull
            .as_ref()
            .and_then(|p| p.pull.get("merged_at"))
            .is_some_and(|v| !v.is_null());
        let head_sha = pull
            .as_ref()
            .and_then(|p| p.pull.pointer("/head/sha"))
            .and_then(Value::as_str);

        let action = if pull.is_none() {
            Action::Create
        } else if merged && head_sha == Some(local.commit_sha.as_str()) {
            Action::SkipMerged
        } else if merged {
            // Commit was amended after its PR merged — re-create.
            Action::Create
        } else if head_sha == Some(local.commit_sha.as_str()) {
            Action::SkipUpToDate
        } else {
            Action::Update
        };

        // If the pull was merged-but-amended we treat it as no-PR
        // for downstream consumers, mirroring Python.
        let pull_for_output = if matches!(action, Action::Create) && pull.is_some() {
            None
        } else {
            pull.map(|p| p.pull)
        };

        locals.push(LocalChange {
            commit_sha: local.commit_sha.clone(),
            title: local.title.clone(),
            message: local.message.clone(),
            change_id: local.change_id.clone(),
            action,
            pull: pull_for_output,
            note: local.note.clone(),
        });
    }

    let orphans: Vec<Value> = remote_changes
        .into_iter()
        .filter_map(|c| {
            let state = c.pull.get("state").and_then(Value::as_str);
            (state == Some("open")).then_some(c.pull)
        })
        .collect();

    Ok(Changes { locals, orphans })
}

/// Same cross-prefix Change-Id matching `sync_status` uses;
/// duplicated here so the two classifiers stay independent.
fn pop_matching(
    remote_changes: &mut Vec<RemoteChange>,
    local_changeid: &str,
) -> Option<RemoteChange> {
    if let Some(idx) = remote_changes
        .iter()
        .position(|c| c.change_id == local_changeid)
    {
        return Some(remote_changes.swap_remove(idx));
    }
    let local_hex = local_changeid.strip_prefix('I').unwrap_or(local_changeid);
    let idx = remote_changes.iter().position(|c| {
        let remote_hex = c.change_id.strip_prefix('I').unwrap_or(&c.change_id);
        remote_hex.starts_with(local_hex) || local_hex.starts_with(remote_hex)
    })?;
    Some(remote_changes.swap_remove(idx))
}

/// First 7 characters of a SHA — the short form every log line
/// uses.
fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// Plain-text log line describing one planned change. `dry_run`
/// selects the would-be wording ("to create") shown in the plan
/// preview vs the past-tense ("created") shown after the push
/// lands. `dest_branch` is the branch the PR will live on — used
/// as the URL placeholder until the PR exists.
///
/// Ported from Python `LocalChange.get_log_from_local_change`,
/// minus the Rich colour tags: this CLI prints plain lines so log
/// scrapers don't have to strip ANSI (see `render_stack_list_text`).
#[must_use]
pub fn format_local_change_log(
    change: &LocalChange,
    dest_branch: &str,
    dry_run: bool,
    create_as_draft: bool,
) -> String {
    let url = match change.pull.as_ref() {
        Some(pull) => pull
            .get("html_url")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>")
            .to_string(),
        None => format!("<{dest_branch}>"),
    };

    let mut flags = String::new();
    let draft = change
        .pull
        .as_ref()
        .and_then(|p| p.get("draft"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if draft || (matches!(change.action, Action::Create) && create_as_draft) {
        flags.push_str(" (draft)");
    }

    let commit_short = short_sha(&change.commit_sha);
    let (action, commit_info) = match change.action {
        Action::Create => (
            if dry_run { "to create" } else { "created" }.to_string(),
            commit_short,
        ),
        Action::Update => {
            let head = change
                .pull
                .as_ref()
                .and_then(|p| p.pointer("/head/sha"))
                .and_then(Value::as_str)
                .map(short_sha)
                .unwrap_or_default();
            (
                if dry_run { "to update" } else { "updated" }.to_string(),
                format!("{head} -> {commit_short}"),
            )
        }
        Action::SkipCreate => (
            "skip, --only-update-existing-pulls".to_string(),
            commit_short,
        ),
        Action::SkipMerged => {
            flags.push_str(" (merged)");
            // The merge commit's short SHA when the PR really merged,
            // else the local commit's. (Python sliced `[7:]` here —
            // a typo; the short SHA is the first 7 chars.)
            let info = change
                .pull
                .as_ref()
                .filter(|p| p.get("merged_at").is_some_and(|v| !v.is_null()))
                .and_then(|p| p.get("merge_commit_sha"))
                .and_then(Value::as_str)
                .map(short_sha)
                .unwrap_or(commit_short);
            ("merged".to_string(), info)
        }
        Action::SkipNextOnly => ("skip, --next-only".to_string(), commit_short),
        Action::SkipUpToDate => ("up-to-date".to_string(), commit_short),
    };

    format!(
        "* [{action}] '{commit_info} - {title}{flags} {url}",
        title = change.title,
    )
}

/// Plain-text log line for an orphan PR (one whose Change-Id left
/// the local stack) being deleted. `dry_run` toggles "to delete"
/// vs "deleted". Ported from
/// `OrphanChange.get_log_from_orphan_change`.
#[must_use]
pub fn format_orphan_change_log(pull: &Value, dry_run: bool) -> String {
    let action = if dry_run { "to delete" } else { "deleted" };
    let title = pull
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>");
    let url = pull
        .get("html_url")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>");
    let sha = pull
        .pointer("/head/sha")
        .and_then(Value::as_str)
        .map_or_else(|| "<unknown>".to_string(), short_sha);
    format!("* [{action}] '{sha} - {title} {url}")
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

    fn pull_merged(change_id: &str, head_sha: &str, number: u64) -> RemoteChange {
        RemoteChange {
            change_id: change_id.to_string(),
            pull: json!({
                "number": number,
                "state": "closed",
                "merged_at": "2025-01-01T00:00:00Z",
                "head": {"sha": head_sha, "ref": "stack/x"},
            }),
        }
    }

    fn pull_open(change_id: &str, head_sha: &str, number: u64) -> RemoteChange {
        RemoteChange {
            change_id: change_id.to_string(),
            pull: json!({
                "number": number,
                "state": "open",
                "merged_at": null,
                "head": {"sha": head_sha, "ref": "stack/x"},
            }),
        }
    }

    #[test]
    fn create_when_no_matching_pull() {
        let cid = "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01";
        let res = classify(&[local("abc", cid, "A")], vec![]).unwrap();
        assert_eq!(res.locals[0].action, Action::Create);
        assert!(res.locals[0].pull.is_none());
    }

    #[test]
    fn skip_merged_when_head_matches() {
        let cid = "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01";
        let res = classify(&[local("abc", cid, "A")], vec![pull_merged(cid, "abc", 1)]).unwrap();
        assert_eq!(res.locals[0].action, Action::SkipMerged);
        assert!(res.locals[0].pull.is_some());
    }

    #[test]
    fn create_when_amended_after_merge() {
        let cid = "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01";
        // PR head = abc, local = def (amended after merge).
        let res = classify(&[local("def", cid, "A")], vec![pull_merged(cid, "abc", 1)]).unwrap();
        assert_eq!(res.locals[0].action, Action::Create);
        // The merged pull is disassociated.
        assert!(res.locals[0].pull.is_none());
    }

    #[test]
    fn skip_up_to_date_when_open_pr_head_matches() {
        let cid = "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01";
        let res = classify(&[local("abc", cid, "A")], vec![pull_open(cid, "abc", 1)]).unwrap();
        assert_eq!(res.locals[0].action, Action::SkipUpToDate);
    }

    #[test]
    fn update_when_open_pr_head_differs() {
        let cid = "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01";
        let res = classify(&[local("def", cid, "A")], vec![pull_open(cid, "abc", 1)]).unwrap();
        assert_eq!(res.locals[0].action, Action::Update);
    }

    #[test]
    fn unmatched_open_pull_becomes_orphan() {
        let res = classify(
            &[],
            vec![pull_open(
                "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01",
                "abc",
                1,
            )],
        )
        .unwrap();
        assert_eq!(res.orphans.len(), 1);
    }

    #[test]
    fn closed_unmatched_pull_is_not_an_orphan() {
        // Merged but unmatched — the user dropped the commit locally
        // and the PR is already closed. Don't surface it.
        let res = classify(
            &[],
            vec![pull_merged(
                "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01",
                "abc",
                1,
            )],
        )
        .unwrap();
        assert!(res.orphans.is_empty());
    }

    fn change(action: Action, pull: Option<Value>) -> LocalChange {
        LocalChange {
            commit_sha: "abc1234def5678".to_string(),
            title: "Add feature".to_string(),
            message: String::new(),
            change_id: "Iaaaa".to_string(),
            action,
            pull,
            note: String::new(),
        }
    }

    #[test]
    fn format_create_plan_uses_branch_placeholder_url() {
        let c = change(Action::Create, None);
        let line = format_local_change_log(&c, "stack/feat/x", true, false);
        assert_eq!(line, "* [to create] 'abc1234 - Add feature <stack/feat/x>");
    }

    #[test]
    fn format_create_done_uses_pull_url_and_draft_flag() {
        let pull = json!({"html_url": "https://gh/pull/7", "draft": true});
        let c = change(Action::Create, Some(pull));
        let line = format_local_change_log(&c, "stack/feat/x", false, false);
        assert_eq!(
            line,
            "* [created] 'abc1234 - Add feature (draft) https://gh/pull/7"
        );
    }

    #[test]
    fn format_create_as_draft_flag_applies_without_pull() {
        let c = change(Action::Create, None);
        let line = format_local_change_log(&c, "stack/feat/x", true, true);
        assert_eq!(
            line,
            "* [to create] 'abc1234 - Add feature (draft) <stack/feat/x>"
        );
    }

    #[test]
    fn format_update_shows_old_to_new_head() {
        let pull = json!({"html_url": "https://gh/pull/7", "head": {"sha": "fff0000aaa"}});
        let c = change(Action::Update, Some(pull));
        let line = format_local_change_log(&c, "stack/feat/x", true, false);
        assert_eq!(
            line,
            "* [to update] 'fff0000 -> abc1234 - Add feature https://gh/pull/7"
        );
    }

    #[test]
    fn format_skip_up_to_date() {
        let pull = json!({"html_url": "https://gh/pull/7", "head": {"sha": "abc1234def5678"}});
        let c = change(Action::SkipUpToDate, Some(pull));
        let line = format_local_change_log(&c, "stack/feat/x", false, false);
        assert_eq!(
            line,
            "* [up-to-date] 'abc1234 - Add feature https://gh/pull/7"
        );
    }

    #[test]
    fn format_skip_merged_uses_merge_commit_and_flag() {
        let pull = json!({
            "html_url": "https://gh/pull/7",
            "merged_at": "2025-01-01T00:00:00Z",
            "merge_commit_sha": "9998887ccc",
        });
        let c = change(Action::SkipMerged, Some(pull));
        let line = format_local_change_log(&c, "stack/feat/x", false, false);
        assert_eq!(
            line,
            "* [merged] '9998887 - Add feature (merged) https://gh/pull/7"
        );
    }

    #[test]
    fn format_orphan_plan_and_deleted() {
        let pull = json!({
            "title": "Old PR",
            "html_url": "https://gh/pull/9",
            "head": {"sha": "deadbeef0000"},
        });
        assert_eq!(
            format_orphan_change_log(&pull, true),
            "* [to delete] 'deadbee - Old PR https://gh/pull/9"
        );
        assert_eq!(
            format_orphan_change_log(&pull, false),
            "* [deleted] 'deadbee - Old PR https://gh/pull/9"
        );
    }
}
