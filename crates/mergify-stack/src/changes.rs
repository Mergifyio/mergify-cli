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
/// `ActionT` literal union.
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
}
