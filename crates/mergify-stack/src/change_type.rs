//! Classify a force-pushed PR head as a *rebase-only* update or a
//! *content* change.
//!
//! Used by `stack push` when building the revision-history table:
//! after the local stack has been rebased onto trunk, every
//! updated PR needs a label so reviewers can distinguish "I just
//! caught up with main" from "I edited the diff." The decision is
//! made by comparing `git patch-id --stable` outputs of the old
//! and new commit SHAs — `patch-id` hashes the diff with line
//! numbers and whitespace normalised, so a pure rebase produces
//! the same id while any real content edit produces a different
//! one.
//!
//! Ported from `mergify_cli/stack/push.py::detect_change_type`
//! plus the [`fetch_old_pr_heads`] helper that materialises the
//! `refs/pull/<n>/head` refs locally before the patch-id
//! comparison runs (without this step the old SHA would already
//! have been clobbered by the force-push). The Python `_internal`
//! bridge that consumes this module lands in a follow-up — this
//! commit ports the leaf so the eventual native `stack push` has
//! it ready.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use mergify_core::CliError;
use serde::{Deserialize, Serialize};

/// Result of [`detect_change_type`] plus the synthetic `Initial`
/// tag used for the first revision-history row (which has no
/// "old" SHA to compare against).
///
/// Wire format matches the Python `Literal["initial", "rebase",
/// "content", "unknown"]`: lowercase strings, no variant
/// prefix — JSON-shape stability is a contract with the
/// revision-history comment parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeType {
    Initial,
    Rebase,
    Content,
    Unknown,
}

impl ChangeType {
    /// Lowercase wire form (`"initial"` / `"rebase"` / `"content"`
    /// / `"unknown"`). Used when emitting the value to anything
    /// that doesn't go through serde (logs, CLI output, the
    /// revision-history comment body).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Rebase => "rebase",
            Self::Content => "content",
            Self::Unknown => "unknown",
        }
    }

    /// Inverse of [`Self::as_str`] with the same fallback semantics
    /// as Python's `_coerce_change_type`: anything that doesn't
    /// match a known variant is coerced to [`Self::Unknown`] so a
    /// legacy or hand-edited comment can't crash the parser.
    #[must_use]
    pub fn from_str_lossy(value: &str) -> Self {
        match value {
            "initial" => Self::Initial,
            "rebase" => Self::Rebase,
            "content" => Self::Content,
            _ => Self::Unknown,
        }
    }
}

/// `git patch-id --stable` on the diff of `sha`.
///
/// Returns the patch-id half of `git patch-id`'s output (the
/// command prints `"<patch-id> <commit-sha>"`). Errors propagate
/// as `CliError::Generic` so callers can decide whether to fall
/// back to [`ChangeType::Unknown`] or surface the failure.
pub fn git_patch_id(repo_dir: Option<&Path>, sha: &str) -> Result<String, CliError> {
    let diff = run_git_capture(repo_dir, &["show", sha])?;

    let mut child = git_cmd(repo_dir)
        .args(["patch-id", "--stable"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| CliError::Generic(format!("failed to spawn `git patch-id`: {e}")))?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| CliError::Generic("failed to open `git patch-id` stdin".to_string()))?;
        stdin
            .write_all(diff.as_bytes())
            .map_err(|e| CliError::Generic(format!("failed to write to `git patch-id`: {e}")))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| CliError::Generic(format!("failed to read `git patch-id` output: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(CliError::Generic(if stderr.is_empty() {
            format!("`git patch-id` failed for {sha}")
        } else {
            stderr
        }));
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|e| CliError::Generic(format!("`git patch-id` output is not UTF-8: {e}")))?;
    stdout
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .ok_or_else(|| CliError::Generic(format!("`git patch-id` printed no patch-id for {sha}")))
}

/// Compare patch-ids of `old_sha` and `new_sha` to classify a
/// force-push.
///
/// Any error — missing commit, non-UTF-8 output, empty patch-id
/// — coerces to [`ChangeType::Unknown`]. Python matches the same
/// `(CommandError, IndexError, UnicodeDecodeError)` net so a flaky
/// classification can't take down a `stack push`.
#[must_use]
pub fn detect_change_type(repo_dir: Option<&Path>, old_sha: &str, new_sha: &str) -> ChangeType {
    let Ok(old) = git_patch_id(repo_dir, old_sha) else {
        return ChangeType::Unknown;
    };
    let Ok(new) = git_patch_id(repo_dir, new_sha) else {
        return ChangeType::Unknown;
    };
    if old == new {
        ChangeType::Rebase
    } else {
        ChangeType::Content
    }
}

/// `git fetch <remote> refs/pull/<n>/head …` so the pre-force-push
/// PR heads exist locally as ref-counted commits.
///
/// Must be called **before** `git push --force-with-lease`
/// overwrites the remote heads — once that happens, the old SHA
/// is reachable only via these `refs/pull/*/head` refs.
///
/// Empty `pr_numbers` is a no-op (mirrors the Python early-return
/// so callers don't need to guard).
pub fn fetch_old_pr_heads(
    repo_dir: Option<&Path>,
    remote: &str,
    pr_numbers: &[u64],
) -> Result<(), CliError> {
    if pr_numbers.is_empty() {
        return Ok(());
    }
    let mut owned: Vec<String> = vec!["fetch".into(), remote.into()];
    for n in pr_numbers {
        owned.push(format!("refs/pull/{n}/head"));
    }
    let args: Vec<&str> = owned.iter().map(String::as_str).collect();
    run_git_silent(repo_dir, &args)
}

fn git_cmd(repo_dir: Option<&Path>) -> Command {
    let mut cmd = Command::new("git");
    if let Some(dir) = repo_dir {
        cmd.arg("-C").arg(dir);
    }
    // Force C locale: callers may match git error messages by
    // English substring, which breaks under translated locales.
    // Mirrors `utils.subprocess_env` on the Python side.
    cmd.env("LC_ALL", "C").env("LANG", "C").env("LANGUAGE", "C");
    cmd
}

fn run_git_capture(repo_dir: Option<&Path>, args: &[&str]) -> Result<String, CliError> {
    let output = git_cmd(repo_dir)
        .args(args)
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
    String::from_utf8(output.stdout).map_err(|e| {
        CliError::Generic(format!("`git {}` output is not UTF-8: {e}", args.join(" ")))
    })
}

fn run_git_silent(repo_dir: Option<&Path>, args: &[&str]) -> Result<(), CliError> {
    let status = git_cmd(repo_dir)
        .args(args)
        .status()
        .map_err(|e| CliError::Generic(format!("failed to spawn `git {}`: {e}", args.join(" "))))?;
    if !status.success() {
        return Err(CliError::Generic(format!(
            "`git {}` exited {status}",
            args.join(" ")
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_repo() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path();
        run(path, &["init", "-q", "-b", "main"]);
        run(path, &["config", "user.email", "test@example.com"]);
        run(path, &["config", "user.name", "Test"]);
        run(path, &["config", "commit.gpgsign", "false"]);
        dir
    }

    fn run(path: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?} failed");
    }

    fn write_and_commit(path: &Path, file: &str, contents: &str, msg: &str) -> String {
        std::fs::write(path.join(file), contents).expect("write");
        run(path, &["add", file]);
        run(path, &["commit", "-q", "-m", msg]);
        run_git_capture(Some(path), &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string()
    }

    #[test]
    fn wire_form_round_trips() {
        for ct in [
            ChangeType::Initial,
            ChangeType::Rebase,
            ChangeType::Content,
            ChangeType::Unknown,
        ] {
            assert_eq!(ChangeType::from_str_lossy(ct.as_str()), ct);
        }
        // Anything unknown coerces to Unknown — guards against
        // legacy / hand-edited comments crashing the parser.
        assert_eq!(ChangeType::from_str_lossy("garbage"), ChangeType::Unknown);
        assert_eq!(ChangeType::from_str_lossy(""), ChangeType::Unknown);
    }

    #[test]
    fn serde_uses_lowercase_strings() {
        // Wire compatibility with the Python `Literal[...]` shape:
        // the revision-history comment embeds the value verbatim,
        // so any drift would break parsing of historic comments.
        assert_eq!(
            serde_json::to_string(&ChangeType::Rebase).unwrap(),
            "\"rebase\""
        );
        assert_eq!(
            serde_json::from_str::<ChangeType>("\"content\"").unwrap(),
            ChangeType::Content
        );
    }

    #[test]
    fn detect_rebase_when_diff_unchanged() {
        let dir = init_repo();
        let path = dir.path();
        write_and_commit(path, "a", "1\n", "base");
        let old = write_and_commit(path, "b", "x\n", "feat: add b");

        // Soft-reset and re-commit the same staged tree with a
        // different message — same diff, new SHA. That's the
        // "pure rebase" signal patch-id is built to detect.
        run(path, &["reset", "--soft", "HEAD~1"]);
        run(path, &["commit", "-q", "-m", "feat: add b (rebased)"]);
        let new = run_git_capture(Some(path), &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();
        assert_ne!(old, new, "soft-reset+commit must produce a new SHA");

        assert_eq!(
            detect_change_type(Some(path), &old, &new),
            ChangeType::Rebase,
        );
    }

    #[test]
    fn detect_content_when_diff_differs() {
        let dir = init_repo();
        let path = dir.path();
        write_and_commit(path, "a", "1\n", "base");
        let old = write_and_commit(path, "b", "x\n", "feat: add b");
        // Amend the same commit to change its content; patch-ids
        // must diverge.
        std::fs::write(path.join("b"), "x\ny\n").unwrap();
        run(path, &["add", "b"]);
        run(path, &["commit", "-q", "--amend", "--no-edit"]);
        let new = run_git_capture(Some(path), &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();

        assert_eq!(
            detect_change_type(Some(path), &old, &new),
            ChangeType::Content,
        );
    }

    #[test]
    fn detect_unknown_when_sha_missing() {
        let dir = init_repo();
        let path = dir.path();
        write_and_commit(path, "a", "1\n", "base");
        // Bogus SHAs — both `git show` calls fail, so the
        // classifier must fall back to Unknown rather than
        // propagating the error.
        assert_eq!(
            detect_change_type(Some(path), "deadbeef", "cafef00d"),
            ChangeType::Unknown,
        );
    }

    #[test]
    fn fetch_old_pr_heads_empty_is_noop() {
        let dir = init_repo();
        // No fetch invocation, so an unconfigured remote can't
        // make this fail.
        fetch_old_pr_heads(Some(dir.path()), "origin", &[]).expect("noop");
    }
}
