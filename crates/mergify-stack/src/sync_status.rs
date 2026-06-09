//! Sync-specific stack classifier.
//!
//! Walks the local stack commits and matches each one to a remote
//! pull request by `Change-Id`. Buckets them into:
//!
//! - **Merged** — the PR is closed-and-merged AND its head SHA
//!   matches the local commit. These can be safely dropped from
//!   the stack: the trunk already contains them.
//! - **Remaining** — everything else (open PR, no PR, amended-
//!   after-merge). These survive the sync rebase.
//!
//! Ported from the merged-vs-remaining slice of
//! `mergify_cli/stack/changes.py::get_changes`. The full
//! create/update/skip-up-to-date classifier needed by
//! `stack push`/`stack list` lands with those slices.

use mergify_core::CliError;
use serde_json::Value;

use crate::local_commits::LocalCommit;
use crate::remote_changes::RemoteChange;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedCommit {
    pub commit_sha: String,
    pub title: String,
    pub pull_number: u64,
    pub pull_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemainingCommit {
    pub commit_sha: String,
    pub title: String,
}

#[derive(Debug, Clone)]
pub struct SyncStatus {
    pub branch: String,
    pub trunk: String,
    pub merged: Vec<MergedCommit>,
    pub remaining: Vec<RemainingCommit>,
}

impl SyncStatus {
    /// True when every commit in the stack has been merged.
    #[must_use]
    pub fn all_merged(&self) -> bool {
        !self.merged.is_empty() && self.remaining.is_empty()
    }

    /// True when there are no merged commits — nothing to rebase away.
    #[must_use]
    pub fn up_to_date(&self) -> bool {
        self.merged.is_empty()
    }
}

/// Classify each local commit. Returns `SyncStatus` with
/// `merged`/`remaining` buckets populated.
///
/// `remote_changes` is consumed: each matched pull is removed
/// from the working list so a Change-Id is never double-counted.
pub fn classify(
    branch: String,
    trunk: String,
    local_commits: &[LocalCommit],
    mut remote_changes: Vec<RemoteChange>,
) -> Result<SyncStatus, CliError> {
    let mut merged = Vec::new();
    let mut remaining = Vec::new();

    for local in local_commits {
        let pull = pop_matching(&mut remote_changes, &local.change_id);

        // Sync only cares about the merged-vs-not bucket — defer
        // the rest of Python's action enum (create/update/skip-up-
        // to-date/…) to the full classifier when stack push lands.
        let is_merged = match &pull {
            Some(p) => {
                let merged_at = p.pull.get("merged_at");
                let head_sha = p.pull.pointer("/head/sha").and_then(Value::as_str);
                merged_at.is_some_and(|v| !v.is_null())
                    && head_sha == Some(local.commit_sha.as_str())
            }
            None => false,
        };

        if is_merged {
            let pull = pull.expect("matched a pull above");
            let pull_number = pull.pull.get("number").and_then(Value::as_u64).unwrap_or(0);
            let pull_url = pull
                .pull
                .get("html_url")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_default();
            merged.push(MergedCommit {
                commit_sha: local.commit_sha.clone(),
                title: local.title.clone(),
                pull_number,
                pull_url,
            });
        } else {
            remaining.push(RemainingCommit {
                commit_sha: local.commit_sha.clone(),
                title: local.title.clone(),
            });
        }
    }

    Ok(SyncStatus {
        branch,
        trunk,
        merged,
        remaining,
    })
}

/// Pop the first remote change whose Change-Id matches `local_changeid`
/// by exact or cross-prefix comparison. Mirrors Python's
/// `pop_remote_change`: handles the asymmetric case where the
/// remote carries a full `I<40hex>` while the local carries the
/// short `<8hex>` form (from new-style branch suffixes) and vice
/// versa.
fn pop_matching(
    remote_changes: &mut Vec<RemoteChange>,
    local_changeid: &str,
) -> Option<RemoteChange> {
    // Exact match first — the common case.
    if let Some(idx) = remote_changes
        .iter()
        .position(|c| c.change_id == local_changeid)
    {
        return Some(remote_changes.swap_remove(idx));
    }
    // Cross-prefix match: strip the leading `I` from both sides
    // and check if either prefixes the other.
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
                "base": {"ref": "main"},
                "html_url": format!("https://github.com/o/r/pull/{number}"),
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
                "base": {"ref": "main"},
                "html_url": format!("https://github.com/o/r/pull/{number}"),
            }),
        }
    }

    #[test]
    fn merged_commit_is_classified_as_merged() {
        let cid = "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01";
        let commits = vec![local("abc123", cid, "feat: A")];
        let remote = vec![pull_merged(cid, "abc123", 42)];
        let status = classify("feat".into(), "origin/main".into(), &commits, remote).unwrap();
        assert_eq!(status.merged.len(), 1);
        assert_eq!(status.merged[0].pull_number, 42);
        assert!(status.remaining.is_empty());
    }

    #[test]
    fn amended_after_merge_stays_in_remaining() {
        let cid = "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01";
        // PR merged at head=abc; local commit is amended to def.
        let commits = vec![local("def456", cid, "feat: A")];
        let remote = vec![pull_merged(cid, "abc123", 42)];
        let status = classify("feat".into(), "origin/main".into(), &commits, remote).unwrap();
        assert!(status.merged.is_empty());
        assert_eq!(status.remaining.len(), 1);
    }

    #[test]
    fn open_pr_stays_remaining() {
        let cid = "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01";
        let commits = vec![local("abc123", cid, "feat: A")];
        let remote = vec![pull_open(cid, "abc123", 42)];
        let status = classify("feat".into(), "origin/main".into(), &commits, remote).unwrap();
        assert!(status.merged.is_empty());
        assert_eq!(status.remaining.len(), 1);
    }

    #[test]
    fn no_matching_pr_stays_remaining() {
        let cid = "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01";
        let commits = vec![local("abc123", cid, "feat: A")];
        let status = classify("feat".into(), "origin/main".into(), &commits, vec![]).unwrap();
        assert!(status.merged.is_empty());
        assert_eq!(status.remaining.len(), 1);
    }

    #[test]
    fn cross_prefix_match_works() {
        // Local has short hex Change-Id (Iaaaaaaaa, 8 hex), remote
        // has full form. The match still succeeds.
        let short = "Iaaaaaaaa";
        let full = "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01";
        let commits = vec![local("abc123", short, "feat: A")];
        let remote = vec![pull_merged(full, "abc123", 42)];
        let status = classify("feat".into(), "origin/main".into(), &commits, remote).unwrap();
        assert_eq!(status.merged.len(), 1);
    }

    #[test]
    fn all_merged_predicate() {
        let cid = "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01";
        let commits = vec![local("abc123", cid, "feat: A")];
        let remote = vec![pull_merged(cid, "abc123", 42)];
        let status = classify("feat".into(), "origin/main".into(), &commits, remote).unwrap();
        assert!(status.all_merged());
        assert!(!status.up_to_date());
    }

    #[test]
    fn up_to_date_predicate_when_no_merged() {
        let cid = "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01";
        let commits = vec![local("abc123", cid, "feat: A")];
        let remote = vec![pull_open(cid, "abc123", 42)];
        let status = classify("feat".into(), "origin/main".into(), &commits, remote).unwrap();
        assert!(!status.all_merged());
        assert!(status.up_to_date());
    }
}
