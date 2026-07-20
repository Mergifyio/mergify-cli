//! Shared git-invocation helpers used by every `commands::*`
//! module + the leaf git-using crates (`change_type`,
//! `notes_push`, `replay`, …).
//!
//! Centralises the locale-forcing (`LC_ALL=C` etc. — several call
//! sites parse English git error messages), the `-C <repo_dir>`
//! prefix, and the "non-zero exit ⇒ `CliError::Generic` with the
//! captured stderr" mapping so the per-command modules don't have
//! to maintain their own slight variations of each helper.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use mergify_core::CliError;

use crate::local_commits::STACK_NOTES_REF;

/// `-c` overrides that make git carry the stack's notes from every
/// rewritten commit onto its replacement.
///
/// A note is addressed by commit SHA, so without this a rebase
/// strands the amend reason on the pre-rebase SHA and the push that
/// follows records a blank `Reason` in the revision history.
/// `mergify stack setup` persists only `notes.rewriteRef` into the
/// repo config — the mode and the rebase toggle are git's defaults
/// there. That config is what covers the rewrites git runs outside
/// this process (`git commit --amend`, and the `git rebase
/// --continue` that finishes a conflicted rebase); passing all three
/// per-invocation keeps the CLI's own rebases correct in repos whose
/// config predates that, and pins the defaults against a global
/// `notes.rewriteMode=ignore` or `notes.rewrite.rebase=false`.
#[must_use]
pub(crate) fn notes_rewrite_config() -> Vec<String> {
    vec![
        "-c".to_string(),
        format!("notes.rewriteRef={STACK_NOTES_REF}"),
        "-c".to_string(),
        "notes.rewriteMode=concatenate".to_string(),
        "-c".to_string(),
        "notes.rewrite.rebase=true".to_string(),
    ]
}

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
///
/// `reapply_cherry_picks` maps to git's `--reapply-cherry-picks`;
/// set it when the editor drops commits by SHA against a base
/// that already contains their squash-merged equivalents.
///
/// Inherits the terminal — git's rebase progress prints live. Use
/// [`spawn_rebase_captured`] when a progress spinner owns the
/// terminal and git's output would corrupt the in-place redraw.
pub fn spawn_rebase(
    repo_dir: &Path,
    base: &str,
    sequence_editor: Option<&str>,
    reapply_cherry_picks: bool,
) -> Result<(), CliError> {
    spawn_rebase_inner(repo_dir, base, sequence_editor, reapply_cherry_picks, false)
}

/// Like [`spawn_rebase`] but captures git's output instead of
/// letting it reach the terminal — for spinner-driven rebases. On
/// success the output is discarded; on failure it's flushed to the
/// real streams first so a conflict is never swallowed.
pub fn spawn_rebase_captured(
    repo_dir: &Path,
    base: &str,
    sequence_editor: Option<&str>,
    reapply_cherry_picks: bool,
) -> Result<(), CliError> {
    spawn_rebase_inner(repo_dir, base, sequence_editor, reapply_cherry_picks, true)
}

fn spawn_rebase_inner(
    repo_dir: &Path,
    base: &str,
    sequence_editor: Option<&str>,
    reapply_cherry_picks: bool,
    capture: bool,
) -> Result<(), CliError> {
    let mut cmd = git_cmd(Some(repo_dir));
    cmd.args(notes_rewrite_config());
    cmd.arg("rebase").arg("-i");
    if reapply_cherry_picks {
        // Keep commits whose patch-id already matches the base in
        // the rebase todo. Git's default (`--no-reapply-cherry-picks`)
        // silently omits them, which breaks callers that hand the
        // rebase-todo rewriter an explicit drop list: the dropped
        // SHAs would have no `pick` line left and the rewrite aborts.
        // Letting them through keeps the drop editor the sole
        // authority on what the rebase removes.
        cmd.arg("--reapply-cherry-picks");
    }
    cmd.arg(base);
    if let Some(editor) = sequence_editor {
        cmd.env("GIT_SEQUENCE_EDITOR", editor);
    }

    let (success, stderr) = if capture {
        // Null stdin so a mid-rebase prompt (e.g. an unexpected
        // interactive editor invocation) fails fast instead of
        // blocking invisibly behind the progress spinner that owns
        // the terminal.
        cmd.stdin(std::process::Stdio::null());
        let output = cmd
            .output()
            .map_err(|e| CliError::Generic(format!("failed to spawn `git rebase -i`: {e}")))?;
        if !output.status.success() {
            // The spinner suppressed git's output, so replay it now
            // that we've hit a failure and the user needs to see it.
            let _ = std::io::stdout().write_all(&output.stdout);
            let _ = std::io::stderr().write_all(&output.stderr);
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        (output.status.success(), stderr)
    } else {
        let success = cmd
            .status()
            .map_err(|e| CliError::Generic(format!("failed to spawn `git rebase -i`: {e}")))?
            .success();
        (success, String::new())
    };

    if !success {
        if rebase_in_progress(repo_dir) {
            // An interrupted rebase left a `rebase-merge`/`rebase-apply`
            // state dir — this is the conflict case. Surface the
            // recovery steps and map to the dedicated Conflict exit
            // code (4) so scripts can branch on it — same contract as
            // the Python `run_scripted_rebase`.
            return Err(CliError::Conflict(
                "rebase failed — there may be conflicts\n\
                 Resolve conflicts then run: git rebase --continue\n\
                 Or abort the rebase with: git rebase --abort"
                    .to_string(),
            ));
        }
        // No rebase in progress: git failed before/while starting
        // (bad base, todo-editor error, …). The continue/abort advice
        // would be misleading, so report the failure plainly.
        return Err(CliError::Generic(if stderr.is_empty() {
            "git rebase failed".to_string()
        } else {
            format!("git rebase failed: {stderr}")
        }));
    }
    Ok(())
}

/// Whether an interrupted rebase left a state directory under the
/// repo's git dir. Distinguishes a real conflict (recoverable with
/// `--continue`/`--abort`) from a rebase that failed before it
/// started, which has no such state.
fn rebase_in_progress(repo_dir: &Path) -> bool {
    let git_dir = run_git_capture(Some(repo_dir), &["rev-parse", "--git-dir"])
        .map_or_else(|_| repo_dir.join(".git"), PathBuf::from);
    // `git rev-parse --git-dir` may return a path relative to
    // `repo_dir`; resolve it against the repo so the existence check
    // works regardless of the process CWD.
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        repo_dir.join(git_dir)
    };
    git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists()
}

/// Base commit SHA of the stack: the merge-base between the trunk
/// ref and `HEAD`.
///
/// `--fork-point` is the precise answer when the reflog has history;
/// falls back to a plain merge-base for fresh clones / CI sandboxes
/// where the reflog is empty. An empty result from both is a
/// [`CliError::StackNotFound`] — the branch shares no history with
/// the trunk.
pub fn compute_base_commit_sha(
    repo_dir: &Path,
    trunk_ref: &str,
    dest_branch: &str,
) -> Result<String, CliError> {
    if let Ok(sha) = run_git_capture(Some(repo_dir), &["merge-base", "--fork-point", trunk_ref])
        && !sha.is_empty()
    {
        return Ok(sha);
    }
    let sha = run_git_capture(Some(repo_dir), &["merge-base", trunk_ref, "HEAD"])?;
    if sha.is_empty() {
        return Err(CliError::StackNotFound(format!(
            "common commit between `{trunk_ref}` and `{dest_branch}` branches not found",
        )));
    }
    Ok(sha)
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

    fn capture(path: &Path, args: &[&str]) -> String {
        run_git_capture(Some(path), args).unwrap()
    }

    /// A repo whose trunk already carries the squash-merged
    /// equivalent of a stacked commit. Returns `(dir, [base_sha,
    /// reapplied_sha, kept_sha])`:
    ///
    /// - `main` has `root` then `feat` (the squash-merge landing).
    /// - `feature` branches off `root`, re-adds the same patch as
    ///   `feat` (so its patch-id matches what's now on `main`), then
    ///   adds a distinct `extra` commit on top.
    ///
    /// Rebasing `feature` onto `main` therefore needs
    /// `--reapply-cherry-picks` to keep the cherry-picked commit in
    /// the todo, or the drop editor has nothing to drop.
    fn squash_merged_repo() -> (TempDir, String, String, String) {
        let dir = init_repo();
        let p = dir.path().to_path_buf();
        let root = capture(&p, &["rev-parse", "HEAD"]);

        // The patch that gets both squash-merged to main and
        // re-created on the feature branch.
        std::fs::write(p.join("feat.txt"), "feature\n").unwrap();
        run(&p, &["add", "feat.txt"]);
        run(&p, &["commit", "-q", "-m", "feat"]);

        // Feature branch off root: re-apply the same patch, then a
        // distinct commit.
        run(&p, &["checkout", "-q", "-b", "feature", &root]);
        std::fs::write(p.join("feat.txt"), "feature\n").unwrap();
        run(&p, &["add", "feat.txt"]);
        run(&p, &["commit", "-q", "-m", "feat (reapplied)"]);
        let reapplied = capture(&p, &["rev-parse", "HEAD"]);
        std::fs::write(p.join("extra.txt"), "extra\n").unwrap();
        run(&p, &["add", "extra.txt"]);
        run(&p, &["commit", "-q", "-m", "extra"]);
        let kept = capture(&p, &["rev-parse", "HEAD"]);

        (dir, root, reapplied, kept)
    }

    #[test]
    fn run_git_capture_returns_trimmed_stdout() {
        let dir = init_repo();
        let out = run_git_capture(Some(dir.path()), &["rev-parse", "HEAD"]).unwrap();
        assert_eq!(out.len(), 40, "SHA1 hex without trailing newline");
    }

    /// Build a sequence editor that copies the rebase todo it's
    /// handed to `todo_copy`, then deletes the reapplied commit's
    /// line so the rebase can complete. Lets a test both assert on
    /// the todo git produced AND drive the drop.
    fn capturing_drop_editor(todo_copy: &Path) -> String {
        // `$1` is the todo path git passes to the editor.
        let copy = todo_copy.to_string_lossy();
        format!("sh -c 'cp \"$1\" \"{copy}\"; sed -i.bak \"/feat (reapplied)/d\" \"$1\"' --")
    }

    #[test]
    fn captured_rebase_drops_squash_merged_commit_via_reapply_cherry_picks() {
        // Regression guard for `--reapply-cherry-picks` on the
        // captured (quiet) rebase path. The flag forces git to keep
        // the cherry-picked (already-on-trunk) commit in the todo so
        // the drop editor has a line to remove; the test asserts the
        // todo contained that line and the commit was dropped while
        // the distinct `extra` commit survived.
        let (dir, _root, _reapplied, kept) = squash_merged_repo();
        let p = dir.path();
        let todo = p.join("captured-todo");
        let editor = capturing_drop_editor(&todo);

        spawn_rebase_captured(p, "main", Some(&editor), true).unwrap();

        let todo_text = std::fs::read_to_string(&todo).unwrap();
        assert!(
            todo_text.contains("feat (reapplied)"),
            "--reapply-cherry-picks must keep the cherry-picked commit \
             in the todo, else there's nothing to drop:\n{todo_text}"
        );
        let subjects = capture(p, &["log", "--format=%s", "main..HEAD"]);
        assert!(
            subjects.contains("extra"),
            "kept commit survives: {subjects}"
        );
        assert!(
            !subjects.contains("feat (reapplied)"),
            "squash-merged commit dropped: {subjects}"
        );
        assert_ne!(capture(p, &["rev-parse", "HEAD"]), kept);
    }

    #[test]
    fn captured_rebase_without_reapply_omits_cherry_picked_from_todo() {
        // The negative side of the guard: dropping
        // `--reapply-cherry-picks` makes git silently omit the
        // already-on-trunk commit from the todo — so a drop editor
        // keyed on it would find nothing. This is exactly the
        // regression the positive test catches.
        let (dir, _root, _reapplied, _kept) = squash_merged_repo();
        let p = dir.path();
        let todo = p.join("captured-todo");
        let editor = capturing_drop_editor(&todo);

        spawn_rebase_captured(p, "main", Some(&editor), false).unwrap();

        let todo_text = std::fs::read_to_string(&todo).unwrap();
        assert!(
            !todo_text.contains("feat (reapplied)"),
            "without --reapply-cherry-picks the cherry-picked commit \
             must be absent from the todo:\n{todo_text}"
        );
    }

    #[test]
    fn rebase_carries_the_stack_note_onto_the_rewritten_commit() {
        // The reason a user attaches with `mergify stack note` lives
        // on the commit SHA, and `stack push` rebases before it reads
        // it back. Without the notes-rewrite config the note stays on
        // the pre-rebase SHA and the revision history records a blank
        // `Reason` — this is that regression.
        let dir = init_repo();
        let p = dir.path();

        run(p, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(p.join("feat.txt"), "feature\n").unwrap();
        run(p, &["add", "feat.txt"]);
        run(p, &["commit", "-q", "-m", "feat"]);
        let notes_ref = format!("--ref={STACK_NOTES_REF}");
        run(p, &["notes", &notes_ref, "add", "-m", "why: review fix"]);
        let before = capture(p, &["rev-parse", "HEAD"]);

        // Move the trunk so the rebase has to rewrite `feat`.
        run(p, &["checkout", "-q", "main"]);
        std::fs::write(p.join("trunk.txt"), "trunk\n").unwrap();
        run(p, &["add", "trunk.txt"]);
        run(p, &["commit", "-q", "-m", "trunk moves"]);
        run(p, &["checkout", "-q", "feature"]);

        spawn_rebase_captured(p, "main", Some("true"), false).unwrap();

        let after = capture(p, &["rev-parse", "HEAD"]);
        assert_ne!(after, before, "the rebase must have rewritten the commit");
        assert_eq!(
            capture(p, &["notes", &notes_ref, "show", &after]),
            "why: review fix"
        );
        // Copied, not moved: the pre-rebase SHA keeps its note, which
        // is what `load_or_seed` reads back off the old PR head.
        assert_eq!(
            capture(p, &["notes", &notes_ref, "show", &before]),
            "why: review fix"
        );
    }

    #[test]
    fn rebase_failure_without_conflict_is_generic_not_conflict() {
        // A rebase that fails before it starts (here: an
        // unresolvable base) must not hand back the conflict
        // continue/abort advice — there is no rebase in progress to
        // continue.
        let dir = init_repo();
        let err = spawn_rebase_captured(dir.path(), "no-such-base", None, false).unwrap_err();
        match err {
            CliError::Generic(msg) => {
                assert!(msg.contains("rebase failed"), "got: {msg}");
            }
            other => panic!("expected Generic, got: {other:?}"),
        }
        // No rebase state dir was left behind.
        assert!(!rebase_in_progress(dir.path()));
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
