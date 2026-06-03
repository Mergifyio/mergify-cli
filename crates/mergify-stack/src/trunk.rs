//! Resolve the "trunk" branch — the `<remote>/<branch>` a stack
//! should land on. Ported from `mergify_cli/utils.py::get_trunk`
//! and the small `git` helpers it leans on.
//!
//! Precedence matches the Python implementation:
//! 1. `branch.<current>.remote` + `branch.<current>.merge` git
//!    config keys (set when the local branch tracks an upstream).
//! 2. Either or both missing → fall back to `origin/HEAD` via
//!    `git symbolic-ref refs/remotes/origin/HEAD`, then *set* the
//!    upstream tracking on the current branch and print a notice.
//! 3. Origin HEAD also missing → `CliError::Generic` so callers can
//!    point the user at `git branch --set-upstream-to`.
//!
//! The set-upstream side effect mirrors `utils.get_trunk` exactly:
//! a Python regression test (`tests/test_utils.py`) pins the
//! behavior, so the Rust port has to preserve it.

use std::path::Path;
use std::process::Command;

use mergify_core::CliError;

/// Outcome of [`get_trunk`] — the resolved `<remote>/<branch>` plus
/// whether tracking was auto-set as a side effect. Callers print a
/// notice when `tracking_set` is true.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trunk {
    pub remote: String,
    pub branch: String,
    /// True when we had to set `branch.<current>.{remote,merge}` as
    /// a side effect (Python prints a yellow "Upstream not set …"
    /// notice in that case).
    pub tracking_set: bool,
    /// The local branch the trunk was resolved for. Carried so the
    /// caller can render the notice without re-querying git.
    pub current_branch: String,
}

impl Trunk {
    /// `<remote>/<branch>` — the form callers pass to `git fetch`
    /// and `git checkout --track -b`.
    #[must_use]
    pub fn refspec(&self) -> String {
        format!("{}/{}", self.remote, self.branch)
    }
}

/// Resolve the trunk for the current branch in *`repo_dir`* (or
/// the process CWD when `None`). See module docs for the
/// precedence chain.
pub fn get_trunk(repo_dir: Option<&Path>) -> Result<Trunk, CliError> {
    let current_branch = git_get_branch_name(repo_dir)
        .map_err(|e| CliError::Generic(format!("can't get the current branch: {e}")))?;

    let target_branch = git_get_target_branch(repo_dir, &current_branch).ok();
    let target_remote = git_get_target_remote(repo_dir, &current_branch).ok();

    if let (Some(branch), Some(remote)) = (target_branch.clone(), target_remote.clone()) {
        return Ok(Trunk {
            remote,
            branch,
            tracking_set: false,
            current_branch,
        });
    }

    let (default_remote, default_branch) = get_default_remote_branch(repo_dir).map_err(|e| {
        CliError::Generic(format!(
            "can't detect the remote target branch for {current_branch}: {e}"
        ))
    })?;

    let branch = target_branch.unwrap_or(default_branch);
    let remote = target_remote.unwrap_or(default_remote);

    git_set_upstream(repo_dir, &current_branch, &remote, &branch).map_err(|e| {
        CliError::Generic(format!("failed to set upstream on {current_branch}: {e}"))
    })?;

    Ok(Trunk {
        remote,
        branch,
        tracking_set: true,
        current_branch,
    })
}

/// `git rev-parse --abbrev-ref HEAD` — the short name of the
/// currently checked out branch.
pub fn git_get_branch_name(repo_dir: Option<&Path>) -> Result<String, CliError> {
    run_git(repo_dir, &["rev-parse", "--abbrev-ref", "HEAD"])
}

/// `branch.<name>.merge` — the upstream ref the branch tracks, with
/// the `refs/heads/` prefix stripped to match Python's behavior.
pub fn git_get_target_branch(repo_dir: Option<&Path>, branch: &str) -> Result<String, CliError> {
    let key = format!("branch.{branch}.merge");
    let value = run_git(repo_dir, &["config", "--get", &key])?;
    Ok(value
        .strip_prefix("refs/heads/")
        .map_or_else(|| value.clone(), str::to_owned))
}

/// `branch.<name>.remote` — the remote the branch tracks.
pub fn git_get_target_remote(repo_dir: Option<&Path>, branch: &str) -> Result<String, CliError> {
    let key = format!("branch.{branch}.remote");
    run_git(repo_dir, &["config", "--get", &key])
}

/// `git symbolic-ref refs/remotes/origin/HEAD` → `(remote, branch)`.
/// Used when the current branch has no upstream tracking set.
fn get_default_remote_branch(repo_dir: Option<&Path>) -> Result<(String, String), CliError> {
    let raw = run_git(repo_dir, &["symbolic-ref", "refs/remotes/origin/HEAD"])?;
    let stripped = raw.strip_prefix("refs/remotes/").unwrap_or(&raw);
    let (remote, branch) = stripped
        .split_once('/')
        .ok_or_else(|| CliError::Generic(format!("unexpected origin/HEAD ref shape: {raw}")))?;
    Ok((remote.to_owned(), branch.to_owned()))
}

fn git_set_upstream(
    repo_dir: Option<&Path>,
    branch: &str,
    remote: &str,
    target_branch: &str,
) -> Result<(), CliError> {
    let upstream = format!("{remote}/{target_branch}");
    run_git(
        repo_dir,
        &["branch", branch, "--set-upstream-to", &upstream],
    )
    .map(|_| ())
}

fn run_git(repo_dir: Option<&Path>, args: &[&str]) -> Result<String, CliError> {
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
        return Err(CliError::Generic(format!(
            "`git {}` failed: {}",
            args.join(" "),
            if stderr.is_empty() {
                "no stderr".to_string()
            } else {
                stderr
            },
        )));
    }
    let stdout = String::from_utf8(output.stdout).map_err(|e| {
        CliError::Generic(format!("`git {}` output is not UTF-8: {e}", args.join(" ")))
    })?;
    Ok(stdout.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    fn init_repo() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "test@example.com"],
            &["config", "user.name", "Test"],
            &["commit", "--allow-empty", "-m", "root"],
        ] {
            let ok = StdCommand::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?} failed");
        }
        dir
    }

    fn run(dir: &Path, args: &[&str]) {
        let ok = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?} failed");
    }

    #[test]
    fn branch_name_returns_short_name() {
        let dir = init_repo();
        let name = git_get_branch_name(Some(dir.path())).unwrap();
        assert_eq!(name, "main");
    }

    #[test]
    fn target_branch_strips_refs_heads_prefix() {
        let dir = init_repo();
        run(
            dir.path(),
            &["config", "branch.main.merge", "refs/heads/develop"],
        );
        let v = git_get_target_branch(Some(dir.path()), "main").unwrap();
        assert_eq!(v, "develop");
    }

    #[test]
    fn target_remote_reads_config() {
        let dir = init_repo();
        run(dir.path(), &["config", "branch.main.remote", "upstream"]);
        let v = git_get_target_remote(Some(dir.path()), "main").unwrap();
        assert_eq!(v, "upstream");
    }

    #[test]
    fn get_trunk_uses_tracking_when_set() {
        let dir = init_repo();
        run(
            dir.path(),
            &["config", "branch.main.merge", "refs/heads/release"],
        );
        run(dir.path(), &["config", "branch.main.remote", "upstream"]);
        let trunk = get_trunk(Some(dir.path())).unwrap();
        assert_eq!(trunk.remote, "upstream");
        assert_eq!(trunk.branch, "release");
        assert!(!trunk.tracking_set);
        assert_eq!(trunk.refspec(), "upstream/release");
    }

    #[test]
    fn get_trunk_falls_back_to_origin_head() {
        // Simulate `origin/HEAD -> origin/main` with a bare upstream.
        let upstream_dir = tempfile::tempdir().unwrap();
        let ok = StdCommand::new("git")
            .arg("-C")
            .arg(upstream_dir.path())
            .args(["init", "-q", "--bare", "-b", "main"])
            .status()
            .unwrap()
            .success();
        assert!(ok);

        let dir = init_repo();
        // Wire up `origin` and seed it with a commit on `main`.
        run(
            dir.path(),
            &[
                "remote",
                "add",
                "origin",
                upstream_dir.path().to_str().unwrap(),
            ],
        );
        run(dir.path(), &["push", "-q", "origin", "main"]);
        // `git remote set-head` writes refs/remotes/origin/HEAD.
        run(dir.path(), &["remote", "set-head", "origin", "main"]);
        // Now create a local branch with no tracking and check it out.
        run(dir.path(), &["checkout", "-q", "-b", "feature"]);

        let trunk = get_trunk(Some(dir.path())).unwrap();
        assert_eq!(trunk.remote, "origin");
        assert_eq!(trunk.branch, "main");
        assert!(trunk.tracking_set);
        assert_eq!(trunk.current_branch, "feature");

        // Side effect: `branch.feature.{remote,merge}` should now be
        // set. Re-running should report `tracking_set = false`.
        let trunk2 = get_trunk(Some(dir.path())).unwrap();
        assert!(!trunk2.tracking_set);
        assert_eq!(trunk2.remote, "origin");
        assert_eq!(trunk2.branch, "main");
    }

    #[test]
    fn get_trunk_errors_when_no_upstream_and_no_origin_head() {
        let dir = init_repo();
        let err = get_trunk(Some(dir.path())).unwrap_err();
        match err {
            CliError::Generic(msg) => {
                assert!(msg.contains("can't detect the remote target branch"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
