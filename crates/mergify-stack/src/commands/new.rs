//! `mergify stack new <name>` — create a new stack branch tracking
//! the resolved trunk (or an explicit `--base <remote>/<branch>`).
//! Port of `mergify_cli/stack/new.py::stack_new`.

use std::path::Path;
use std::process::Command;

use mergify_core::CliError;

use crate::trunk;

/// Base ref the new branch should fork from. `None` means "resolve
/// it via [`trunk::get_trunk`] using the current branch's tracking
/// info or `origin/HEAD`".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Base {
    pub remote: String,
    pub branch: String,
}

impl Base {
    fn refspec(&self) -> String {
        format!("{}/{}", self.remote, self.branch)
    }
}

/// Side-effect log we surface to the human-mode caller so it can
/// render the same messages the Python implementation did. Keeps
/// `run` itself printing-free (callers handle stdout/stderr).
#[derive(Debug, Default, Clone)]
pub struct Outcome {
    /// Set when `get_trunk` had to write `branch.<current>.{remote,merge}`
    /// — Python prints a yellow "Upstream not set …" notice in
    /// that case.
    pub upstream_auto_set: Option<UpstreamAutoSet>,
    pub branch_name: String,
    pub base_refspec: String,
    pub checked_out: bool,
}

#[derive(Debug, Clone)]
pub struct UpstreamAutoSet {
    pub current_branch: String,
    pub remote: String,
    pub branch: String,
}

/// Create a new stack branch.
///
/// `name` — the branch to create.
/// `base` — `Some((remote, branch))` to fork from an explicit ref,
///   `None` to resolve the trunk (and lazily set tracking on the
///   current branch as a side effect, mirroring Python).
/// `checkout` — when true, `git checkout --track -b`; otherwise
///   `git branch --track` and the user is left on the original
///   branch.
///
/// Errors:
/// - [`CliError::StackNotFound`] if the trunk can't be resolved
///   (no upstream tracking AND no `origin/HEAD`). Matches Python's
///   `sys.exit(ExitCode.STACK_NOT_FOUND)`.
/// - [`CliError::Generic`] for `git fetch` / `git checkout` /
///   `git branch` failures. Matches Python's
///   `sys.exit(ExitCode.GENERIC_ERROR)` after a failed branch
///   create, plus the raise on a failed fetch.
pub fn run(
    repo_dir: Option<&Path>,
    name: &str,
    base: Option<Base>,
    checkout: bool,
) -> Result<Outcome, CliError> {
    let (base, upstream_auto_set) = if let Some(b) = base {
        (b, None)
    } else {
        let trunk = trunk::get_trunk(repo_dir).map_err(|e| {
            // Preserve the underlying error so users see *why* the
            // trunk couldn't be resolved (missing `origin/HEAD`,
            // failed `git branch --set-upstream-to`, not a git
            // repo, …). The wrapper exit code stays
            // `STACK_NOT_FOUND`, matching the Python flow.
            CliError::StackNotFound(format!(
                "could not determine trunk branch ({e}). Please set \
                 upstream tracking or use --base to specify the base \
                 branch."
            ))
        })?;
        let auto_set = trunk.tracking_set.then(|| UpstreamAutoSet {
            current_branch: trunk.current_branch.clone(),
            remote: trunk.remote.clone(),
            branch: trunk.branch.clone(),
        });
        (
            Base {
                remote: trunk.remote,
                branch: trunk.branch,
            },
            auto_set,
        )
    };

    run_git(repo_dir, &["fetch", &base.remote, &base.branch]).map_err(|e| {
        CliError::Generic(format!(
            "failed to fetch from {remote}: {e}",
            remote = base.remote
        ))
    })?;

    let base_ref = base.refspec();
    let create_args: Vec<&str> = if checkout {
        vec!["checkout", "--track", "-b", name, &base_ref]
    } else {
        vec!["branch", "--track", name, &base_ref]
    };
    run_git(repo_dir, &create_args)
        .map_err(|e| CliError::Generic(format!("failed to create branch '{name}': {e}")))?;

    Ok(Outcome {
        upstream_auto_set,
        branch_name: name.to_string(),
        base_refspec: base_ref,
        checked_out: checkout,
    })
}

fn run_git(repo_dir: Option<&Path>, args: &[&str]) -> Result<(), CliError> {
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    /// Build `<workdir>` containing two repos:
    /// - `upstream.git` — a bare repo with `main` (one commit)
    /// - `local`       — a clone of `upstream.git` with `origin` wired up
    fn make_repo_pair() -> TempDir {
        let workdir = tempfile::tempdir().unwrap();

        let upstream = workdir.path().join("upstream.git");
        run_status(&[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            upstream.to_str().unwrap(),
        ]);

        let seed = workdir.path().join("seed");
        std::fs::create_dir(&seed).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@e.com"],
            &["config", "user.name", "T"],
            &["commit", "--allow-empty", "-m", "root"],
            &["remote", "add", "origin", upstream.to_str().unwrap()],
            &["push", "-q", "origin", "main"],
        ] {
            run_in(&seed, args);
        }

        let local = workdir.path().join("local");
        run_status(&[
            "clone",
            "-q",
            upstream.to_str().unwrap(),
            local.to_str().unwrap(),
        ]);
        run_in(&local, &["config", "user.email", "t@e.com"]);
        run_in(&local, &["config", "user.name", "T"]);
        workdir
    }

    fn local_path(workdir: &Path) -> std::path::PathBuf {
        workdir.join("local")
    }

    fn run_in(dir: &Path, args: &[&str]) {
        let ok = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git -C {dir:?} {args:?} failed");
    }

    fn run_status(args: &[&str]) {
        let ok = StdCommand::new("git")
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?} failed");
    }

    fn branch_exists(dir: &Path, name: &str) -> bool {
        StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "--verify", "--quiet", name])
            .status()
            .unwrap()
            .success()
    }

    fn current_branch(dir: &Path) -> String {
        let out = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn creates_branch_with_checkout_using_trunk() {
        let work = make_repo_pair();
        let local = local_path(work.path());

        let outcome = run(Some(&local), "feature-x", None, true).unwrap();
        assert_eq!(outcome.branch_name, "feature-x");
        assert_eq!(outcome.base_refspec, "origin/main");
        assert!(outcome.checked_out);
        assert!(branch_exists(&local, "feature-x"));
        assert_eq!(current_branch(&local), "feature-x");
    }

    #[test]
    fn creates_branch_without_checkout() {
        let work = make_repo_pair();
        let local = local_path(work.path());

        let outcome = run(Some(&local), "feature-y", None, false).unwrap();
        assert!(!outcome.checked_out);
        assert!(branch_exists(&local, "feature-y"));
        assert_eq!(current_branch(&local), "main");
    }

    #[test]
    fn creates_branch_from_explicit_base() {
        let work = make_repo_pair();
        let local = local_path(work.path());
        // Add a second branch to the upstream so we have a non-main
        // base to fork from.
        let upstream = work.path().join("upstream.git");
        // Push a "develop" branch from the local clone.
        run_in(&local, &["checkout", "-q", "-b", "develop"]);
        run_in(&local, &["commit", "--allow-empty", "-m", "dev"]);
        run_in(&local, &["push", "-q", "origin", "develop"]);
        run_in(&local, &["checkout", "-q", "main"]);
        run_in(&local, &["branch", "-q", "-D", "develop"]);
        // sanity
        assert!(
            StdCommand::new("git")
                .arg("-C")
                .arg(&upstream)
                .args(["rev-parse", "--verify", "develop"])
                .status()
                .unwrap()
                .success()
        );

        let outcome = run(
            Some(&local),
            "feature-z",
            Some(Base {
                remote: "origin".to_string(),
                branch: "develop".to_string(),
            }),
            true,
        )
        .unwrap();
        assert_eq!(outcome.base_refspec, "origin/develop");
        assert!(branch_exists(&local, "feature-z"));
    }

    #[test]
    fn errors_when_branch_already_exists() {
        let work = make_repo_pair();
        let local = local_path(work.path());
        run_in(&local, &["branch", "existing"]);

        let err = run(Some(&local), "existing", None, true).unwrap_err();
        match err {
            CliError::Generic(msg) => {
                assert!(
                    msg.contains("already exists") || msg.contains("existing"),
                    "unexpected error: {msg}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn errors_when_trunk_cannot_be_resolved() {
        // Repo with no remotes at all -> origin/HEAD doesn't exist
        // and the current branch has no upstream.
        let dir = tempfile::tempdir().unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@e.com"],
            &["config", "user.name", "T"],
            &["commit", "--allow-empty", "-m", "root"],
        ] {
            run_in(dir.path(), args);
        }

        let err = run(Some(dir.path()), "feature", None, true).unwrap_err();
        match err {
            CliError::StackNotFound(msg) => {
                assert!(msg.contains("could not determine trunk branch"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
