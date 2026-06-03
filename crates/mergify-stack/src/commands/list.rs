//! `mergify stack list [--json] [--verbose]` — show the current
//! stack's commits and their associated PRs, optionally with CI
//! and review status.
//!
//! Port of `mergify_cli/stack/list.py::stack_list`. Uses the
//! shared [`crate::changes::classify`] for the per-commit
//! create/update/skip-merged/skip-up-to-date classification,
//! then optionally fans out per-PR `check-runs` and `reviews`
//! fetches the same way Python does.

use std::path::{Path, PathBuf};
use std::process::Command;

use mergify_core::CliError;
use mergify_core::HttpClient;
use serde::Serialize;
use serde_json::Value;

use crate::changes::{self, Action};
use crate::local_commits;
use crate::remote_changes;
use crate::stack_context;
use crate::trunk;

/// Per-commit summary used by `stack list` / `stack open`. The
/// shape mirrors Python's `StackListEntry.to_dict()` 1:1 so the
/// `--json` output is byte-compatible with the pre-port version.
#[derive(Debug, Clone, Serialize)]
pub struct StackListEntry {
    pub commit_sha: String,
    pub title: String,
    pub change_id: String,
    /// `"open" | "draft" | "merged" | "no_pr" | "skipped"`.
    pub status: String,
    pub pull_number: Option<u64>,
    pub pull_url: Option<String>,
    pub ci_status: String,
    pub ci_checks: Vec<CiCheck>,
    pub review_status: String,
    pub reviews: Vec<Review>,
    pub mergeable: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CiCheck {
    pub name: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Review {
    pub user: String,
    pub state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StackListOutput {
    pub branch: String,
    pub trunk: String,
    pub entries: Vec<StackListEntry>,
}

pub struct Options<'a> {
    pub repo_dir: Option<&'a Path>,
    pub client: &'a HttpClient,
    pub user: &'a str,
    pub repo: &'a str,
    pub author: &'a str,
    pub branch_prefix: &'a str,
    pub trunk: (&'a str, &'a str),
    /// When false, skip the `/check-runs` and `/reviews` fetches
    /// — `stack open` uses this since it only needs the URL.
    pub include_status: bool,
}

pub async fn run(opts: &Options<'_>) -> Result<StackListOutput, CliError> {
    let repo_dir = resolve_repo_toplevel(opts.repo_dir)?;
    let dest_branch = trunk::git_get_branch_name(Some(&repo_dir))?;
    stack_context::check_local_branch(&dest_branch, opts.branch_prefix)?;

    let (remote, base_branch) = opts.trunk;
    if base_branch == dest_branch {
        return Err(CliError::InvalidState(format!(
            "your local branch `{dest_branch}` targets itself: `{remote}/{base_branch}`. \
             Either fix the target branch (`git branch {dest_branch} \
             --set-upstream-to=<remote>/<target>`) or rename it."
        )));
    }

    let stack_prefix = if opts.branch_prefix.is_empty() {
        dest_branch.clone()
    } else {
        format!("{prefix}/{dest_branch}", prefix = opts.branch_prefix)
    };

    let trunk_ref = format!("{remote}/{base_branch}");
    let base_commit_sha =
        match run_git_capture(Some(&repo_dir), &["merge-base", "--fork-point", &trunk_ref]) {
            Ok(sha) if !sha.is_empty() => sha,
            _ => run_git_capture(Some(&repo_dir), &["merge-base", &trunk_ref, "HEAD"])?,
        };
    if base_commit_sha.is_empty() {
        return Err(CliError::StackNotFound(format!(
            "common commit between `{trunk_ref}` and `{dest_branch}` branches not found"
        )));
    }

    let remote_changes = remote_changes::get_remote_changes(
        opts.client,
        opts.user,
        opts.repo,
        &stack_prefix,
        opts.author,
    )
    .await?;
    let local = local_commits::read(&repo_dir, &base_commit_sha, "HEAD")?;
    let classified = changes::classify(&local, remote_changes)?;

    let mut entries = Vec::with_capacity(classified.locals.len());
    for local in &classified.locals {
        let (status, pull_number, pull_url, mergeable) = match &local.pull {
            None => ("no_pr".to_string(), None, None, None),
            Some(pull) => {
                let merged = pull.get("merged_at").is_some_and(|v| !v.is_null());
                let draft = pull.get("draft").and_then(Value::as_bool).unwrap_or(false);
                let status = if matches!(local.action, Action::SkipMerged) || merged {
                    "merged"
                } else if draft {
                    "draft"
                } else {
                    "open"
                };
                let pull_number = pull.get("number").and_then(Value::as_u64);
                let pull_url = pull
                    .get("html_url")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let mergeable = pull.get("mergeable").and_then(Value::as_bool);
                (status.to_string(), pull_number, pull_url, mergeable)
            }
        };
        entries.push(StackListEntry {
            commit_sha: local.commit_sha.clone(),
            title: local.title.clone(),
            change_id: local.change_id.clone(),
            status,
            pull_number,
            pull_url,
            ci_status: "unknown".to_string(),
            ci_checks: Vec::new(),
            review_status: "unknown".to_string(),
            reviews: Vec::new(),
            mergeable,
        });
    }

    if opts.include_status {
        fetch_pr_details(
            opts.client,
            opts.user,
            opts.repo,
            &classified.locals,
            &mut entries,
        )
        .await?;
    }

    Ok(StackListOutput {
        branch: dest_branch,
        trunk: trunk_ref,
        entries,
    })
}

/// Per-PR fan-out: for each entry with a pull, fetch
/// `/check-runs` + `/reviews` and fold them into the entry's
/// `ci_*` / `review_*` fields. Sequential rather than concurrent
/// since GitHub's secondary rate limit kicks in around ~80
/// concurrent calls on the same endpoint pool — and a typical
/// stack has well under that.
async fn fetch_pr_details(
    client: &HttpClient,
    user: &str,
    repo: &str,
    classified: &[crate::changes::LocalChange],
    entries: &mut [StackListEntry],
) -> Result<(), CliError> {
    for (i, change) in classified.iter().enumerate() {
        let Some(pull) = &change.pull else { continue };
        let Some(head_sha) = pull.pointer("/head/sha").and_then(Value::as_str) else {
            continue;
        };
        let Some(pull_number) = pull.get("number").and_then(Value::as_u64) else {
            continue;
        };

        let checks_path = format!("/repos/{user}/{repo}/commits/{head_sha}/check-runs");
        let reviews_path = format!("/repos/{user}/{repo}/pulls/{pull_number}/reviews");

        let checks_payload: Value = client.get(&checks_path).await?;
        let reviews_payload: Value = client.get(&reviews_path).await?;

        let check_runs: Vec<Value> = checks_payload
            .get("check_runs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let (ci_status, ci_checks) = compute_ci_status(&check_runs);
        entries[i].ci_status = ci_status.to_string();
        entries[i].ci_checks = ci_checks;

        let reviews: Vec<Value> = reviews_payload.as_array().cloned().unwrap_or_default();
        let (review_status, review_list) = compute_review_status(&reviews);
        entries[i].review_status = review_status.to_string();
        entries[i].reviews = review_list;
    }
    Ok(())
}

fn compute_ci_status(check_runs: &[Value]) -> (&'static str, Vec<CiCheck>) {
    if check_runs.is_empty() {
        return ("unknown", Vec::new());
    }
    let mut checks = Vec::with_capacity(check_runs.len());
    let mut has_pending = false;
    let mut has_failure = false;
    for run in check_runs {
        let name = run
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let status = run.get("status").and_then(Value::as_str);
        let conclusion = run.get("conclusion").and_then(Value::as_str);
        let kind = if status != Some("completed") {
            has_pending = true;
            "pending"
        } else if matches!(conclusion, Some("success" | "skipped")) {
            "success"
        } else {
            has_failure = true;
            "failure"
        };
        checks.push(CiCheck {
            name,
            status: kind.to_string(),
        });
    }
    let overall = if has_failure {
        "failing"
    } else if has_pending {
        "pending"
    } else {
        "passing"
    };
    (overall, checks)
}

fn compute_review_status(reviews: &[Value]) -> (&'static str, Vec<Review>) {
    if reviews.is_empty() {
        return ("unknown", Vec::new());
    }
    // Mirrors Python: keep the latest per user; APPROVED /
    // CHANGES_REQUESTED / DISMISSED override an earlier COMMENTED.
    let mut latest_by_user: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for review in reviews {
        let user = review
            .pointer("/user/login")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if user.is_empty() {
            continue;
        }
        let state = review
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let promoting = matches!(
            state.as_str(),
            "APPROVED" | "CHANGES_REQUESTED" | "DISMISSED"
        );
        if promoting || !latest_by_user.contains_key(&user) {
            latest_by_user.insert(user, state);
        }
    }
    let list: Vec<Review> = latest_by_user
        .into_iter()
        .map(|(user, state)| Review { user, state })
        .collect();
    let has_changes = list.iter().any(|r| r.state == "CHANGES_REQUESTED");
    let has_approved = list.iter().any(|r| r.state == "APPROVED");
    let overall = if has_changes {
        "changes_requested"
    } else if has_approved {
        "approved"
    } else {
        "pending"
    };
    (overall, list)
}

fn resolve_repo_toplevel(repo_dir: Option<&Path>) -> Result<PathBuf, CliError> {
    let raw = run_git_capture(repo_dir, &["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(raw))
}

fn run_git_capture(repo_dir: Option<&Path>, args: &[&str]) -> Result<String, CliError> {
    let mut cmd = Command::new("git");
    if let Some(dir) = repo_dir {
        cmd.arg("-C").arg(dir);
    }
    cmd.args(args);
    let output = cmd
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
    let stdout = String::from_utf8(output.stdout).map_err(|e| {
        CliError::Generic(format!("`git {}` output is not UTF-8: {e}", args.join(" ")))
    })?;
    Ok(stdout.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compute_ci_status_classifies_passing() {
        let runs = vec![
            json!({"name": "ci-1", "status": "completed", "conclusion": "success"}),
            json!({"name": "ci-2", "status": "completed", "conclusion": "skipped"}),
        ];
        let (status, checks) = compute_ci_status(&runs);
        assert_eq!(status, "passing");
        assert_eq!(checks.len(), 2);
        assert!(checks.iter().all(|c| c.status == "success"));
    }

    #[test]
    fn compute_ci_status_classifies_failing_over_pending() {
        let runs = vec![
            json!({"name": "ci-1", "status": "in_progress"}),
            json!({"name": "ci-2", "status": "completed", "conclusion": "failure"}),
        ];
        let (status, _) = compute_ci_status(&runs);
        assert_eq!(status, "failing");
    }

    #[test]
    fn compute_ci_status_classifies_pending() {
        let runs = vec![json!({"name": "ci-1", "status": "queued"})];
        let (status, _) = compute_ci_status(&runs);
        assert_eq!(status, "pending");
    }

    #[test]
    fn compute_review_status_latest_per_user_promoting_states() {
        let reviews = vec![
            json!({"user": {"login": "alice"}, "state": "COMMENTED"}),
            json!({"user": {"login": "alice"}, "state": "APPROVED"}),
            json!({"user": {"login": "bob"}, "state": "CHANGES_REQUESTED"}),
        ];
        let (status, list) = compute_review_status(&reviews);
        assert_eq!(status, "changes_requested");
        assert_eq!(list.len(), 2);
        let by_user: std::collections::HashMap<_, _> =
            list.into_iter().map(|r| (r.user, r.state)).collect();
        assert_eq!(by_user["alice"], "APPROVED");
        assert_eq!(by_user["bob"], "CHANGES_REQUESTED");
    }

    #[test]
    fn compute_review_status_approved_when_no_changes_requested() {
        let reviews = vec![json!({"user": {"login": "alice"}, "state": "APPROVED"})];
        let (status, _) = compute_review_status(&reviews);
        assert_eq!(status, "approved");
    }

    #[test]
    fn compute_review_status_pending_when_only_comments() {
        let reviews = vec![json!({"user": {"login": "alice"}, "state": "COMMENTED"})];
        let (status, _) = compute_review_status(&reviews);
        assert_eq!(status, "pending");
    }
}
