//! Shared git-invocation helpers used by every `commands::*`
//! module + the leaf git-using crates (`change_type`,
//! `notes_push`, `replay`, …).
//!
//! Centralises the locale-forcing (`LC_ALL=C` etc. — several call
//! sites parse English git error messages), the `-C <repo_dir>`
//! prefix, and the "non-zero exit ⇒ `CliError::Generic` with the
//! captured stderr" mapping so the per-command modules don't have
//! to maintain their own slight variations of each helper.

use std::path::{Path, PathBuf};
use std::process::Command;

use mergify_core::CliError;

/// Base `git` `Command` with `-C <repo_dir>` (when supplied) and a
/// forced C locale. Use for any git invocation whose stderr or
/// stdout the caller might parse — translated locales would
/// otherwise break the substring matches `change_type`,
/// `notes_push`, and the rebase helpers rely on.
#[must_use]
pub fn git_cmd(repo_dir: Option<&Path>) -> Command {
    let mut cmd = Command::new("git");
    if let Some(dir) = repo_dir {
        cmd.arg("-C").arg(dir);
    }
    cmd.env("LC_ALL", "C").env("LANG", "C").env("LANGUAGE", "C");
    cmd
}

/// Run `git <args>` and return trimmed stdout.
///
/// Non-zero exit surfaces the trimmed stderr (or a generic
/// `git <args> failed` when stderr is empty) wrapped in
/// [`CliError::Generic`]; non-UTF-8 stdout is the same error
/// class.
pub fn run_git_capture(repo_dir: Option<&Path>, args: &[&str]) -> Result<String, CliError> {
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
    let stdout = String::from_utf8(output.stdout).map_err(|e| {
        CliError::Generic(format!("`git {}` output is not UTF-8: {e}", args.join(" ")))
    })?;
    Ok(stdout.trim_end().to_string())
}

/// Run `git <args>` discarding stdout. Useful for `fetch`, `push`,
/// `update-ref` and friends where the caller only cares about
/// success.
pub fn run_git_silent(repo_dir: Option<&Path>, args: &[&str]) -> Result<(), CliError> {
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
    Ok(())
}

/// `git rev-parse --show-toplevel` resolved to a `PathBuf`. Almost
/// every stack subcommand needs this on entry so the rest of the
/// flow operates on an absolute repo root regardless of the
/// user's CWD.
pub fn resolve_repo_toplevel(repo_dir: Option<&Path>) -> Result<PathBuf, CliError> {
    Ok(PathBuf::from(run_git_capture(
        repo_dir,
        &["rev-parse", "--show-toplevel"],
    )?))
}

/// `git rebase -i <base>` with `GIT_SEQUENCE_EDITOR` pointing at
/// the caller-built editor command (or with the user's editor
/// when `sequence_editor` is `None` — the interactive `stack
/// edit` path). Used by every rebase-driving stack subcommand
/// to dispatch back into the rebase-todo rewriter without
/// spawning a real interactive editor (or falling through to
/// it when the caller wants the user in the loop).
pub fn spawn_rebase(
    repo_dir: &Path,
    base: &str,
    sequence_editor: Option<&str>,
) -> Result<(), CliError> {
    let mut cmd = git_cmd(Some(repo_dir));
    cmd.args(["rebase", "-i", base]);
    if let Some(editor) = sequence_editor {
        cmd.env("GIT_SEQUENCE_EDITOR", editor);
    }
    let status = cmd
        .status()
        .map_err(|e| CliError::Generic(format!("failed to spawn `git rebase -i`: {e}")))?;
    if !status.success() {
        return Err(CliError::Generic(format!(
            "`git rebase -i {base}` exited {status}"
        )));
    }
    Ok(())
}

/// POSIX shell single-quote a value so it survives substitution
/// into `GIT_SEQUENCE_EDITOR=…`. Doubles existing single quotes
/// via the standard `'\''` trick.
#[must_use]
pub fn shell_quote(value: &str) -> String {
    let escaped = value.replace('\'', "'\\''");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn run(path: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn init_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        let p = dir.path();
        run(p, &["init", "-q", "-b", "main"]);
        run(p, &["config", "user.email", "t@e.com"]);
        run(p, &["config", "user.name", "t"]);
        run(p, &["config", "commit.gpgsign", "false"]);
        std::fs::write(p.join("x"), "x\n").unwrap();
        run(p, &["add", "x"]);
        run(p, &["commit", "-q", "-m", "init"]);
        dir
    }

    #[test]
    fn run_git_capture_returns_trimmed_stdout() {
        let dir = init_repo();
        let out = run_git_capture(Some(dir.path()), &["rev-parse", "HEAD"]).unwrap();
        assert_eq!(out.len(), 40, "SHA1 hex without trailing newline");
    }

    #[test]
    fn run_git_capture_surfaces_stderr_on_failure() {
        let dir = init_repo();
        let err = run_git_capture(Some(dir.path()), &["rev-parse", "--verify", "no-such-ref"])
            .unwrap_err();
        let CliError::Generic(msg) = err else {
            panic!("expected Generic");
        };
        // git's stderr for a bad ref is e.g.
        // "fatal: Needed a single revision" — assert on the
        // generic "fatal:" prefix so the test isn't tied to the
        // exact wording across git versions.
        assert!(msg.contains("fatal"), "stderr passed through: {msg}");
    }

    #[test]
    fn resolve_repo_toplevel_returns_repo_root() {
        let dir = init_repo();
        let resolved = resolve_repo_toplevel(Some(dir.path())).unwrap();
        // Canonicalise both to dodge macOS `/private/var/folders/...`
        // realpath divergence on tmp paths.
        assert_eq!(
            std::fs::canonicalize(&resolved).unwrap(),
            std::fs::canonicalize(dir.path()).unwrap(),
        );
    }

    #[test]
    fn shell_quote_escapes_embedded_single_quotes() {
        assert_eq!(shell_quote("simple"), "'simple'");
        assert_eq!(shell_quote("with 'quote'"), r"'with '\''quote'\'''");
        // Empty input still produces a valid empty-quoted token.
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn git_cmd_forces_c_locale_for_predictable_error_parsing() {
        // The locale env-vars are critical — several call sites
        // (notes_push, change_type) parse English git error
        // messages and would break under translated locales.
        let cmd = git_cmd(None);
        let envs: std::collections::HashMap<_, _> = cmd
            .get_envs()
            .map(|(k, v)| (k.to_owned(), v.map(std::ffi::OsStr::to_owned)))
            .collect();
        assert_eq!(
            envs[std::ffi::OsStr::new("LC_ALL")].as_deref(),
            Some(std::ffi::OsStr::new("C"))
        );
        assert_eq!(
            envs[std::ffi::OsStr::new("LANG")].as_deref(),
            Some(std::ffi::OsStr::new("C"))
        );
        assert_eq!(
            envs[std::ffi::OsStr::new("LANGUAGE")].as_deref(),
            Some(std::ffi::OsStr::new("C")),
        );
    }
}
