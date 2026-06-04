//! `mergify stack note [<commit>] [-m <msg>] [--append] [--remove]`
//! — attach a "why was this commit amended" note to a commit on
//! `refs/notes/mergify/stack`. Port of
//! `mergify_cli/stack/note.py::stack_note`.
//!
//! The note ref is what `mergify stack push` reads when it renders
//! a PR's "Revision history" comment — see the `note` field on
//! [`crate::local_commits::LocalCommit`] for the read-side
//! counterpart.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use mergify_core::CliError;

use crate::change_id;
use crate::local_commits::{self, STACK_NOTES_REF};
use crate::trunk;

const EDITOR_TEMPLATE: &str =
    "\n# Why was this commit amended? Lines starting with # are ignored.\n";

/// What [`run`] should do with the target commit's note.
#[derive(Debug, Clone)]
pub enum Action {
    /// Replace any existing note with `message`. The default
    /// for `mergify stack note -m "…"` and the post-editor path.
    Set(String),
    /// Concatenate `message` to the existing note with a blank
    /// line separator (`git notes append` semantics).
    Append(String),
    /// Read the note from `$GIT_EDITOR` (then `VISUAL`, `EDITOR`,
    /// `vi`) using a template that includes the "lines starting
    /// with # are ignored" hint.
    FromEditor,
    /// Remove the existing note. No-op (and returns
    /// [`Outcome::NoNoteToRemove`]) when the commit has no note.
    Remove,
}

/// Result of [`run`] — callers print one human line per variant.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// Note attached/replaced/appended successfully.
    Attached { sha: String, subject: String },
    /// `--remove` was given and a note existed before; now gone.
    Removed { sha: String, subject: String },
    /// `--remove` was given on a commit with no note. Python prints
    /// `No note on <sha> <subject>.` and exits 0; mirror that.
    NoNoteToRemove { sha: String, subject: String },
}

/// Attach, append, or remove the note on the target commit.
///
/// `commit` resolution chain (mirrors Python):
/// - `None` → `HEAD`
/// - looks like a Change-Id prefix (`I[0-9a-f]+`) → walk the stack
///   (`<merge-base trunk HEAD>..HEAD`) and match by `change_id`
/// - otherwise → `git rev-parse --verify <commit>^{commit}`
///
/// Errors:
/// - [`CliError::InvalidState`] for an empty message (inline `-m ""`
///   or editor returning only comment lines), and for an
///   ambiguous / missing Change-Id prefix match. Matches Python's
///   `sys.exit(ExitCode.INVALID_STATE)` flow.
/// - [`CliError::Generic`] for git invocation failures and editor
///   process errors.
pub fn run(
    repo_dir: Option<&Path>,
    commit: Option<&str>,
    action: Action,
) -> Result<Outcome, CliError> {
    let (sha, subject) = resolve_commit(repo_dir, commit)?;

    match action {
        Action::Remove => {
            // Mirror Python: if there's no note, print a message
            // and exit 0; otherwise remove and confirm.
            if note_exists(repo_dir, &sha)? {
                run_git(repo_dir, &["notes", &notes_ref_arg(), "remove", &sha])?;
                Ok(Outcome::Removed { sha, subject })
            } else {
                Ok(Outcome::NoNoteToRemove { sha, subject })
            }
        }
        Action::FromEditor => {
            let message = read_note_from_editor()?;
            attach_note(repo_dir, &sha, &message, /*append=*/ false)?;
            Ok(Outcome::Attached { sha, subject })
        }
        Action::Set(message) => {
            let message = sanitize_message(&message)?;
            attach_note(repo_dir, &sha, &message, /*append=*/ false)?;
            Ok(Outcome::Attached { sha, subject })
        }
        Action::Append(message) => {
            let message = sanitize_message(&message)?;
            attach_note(repo_dir, &sha, &message, /*append=*/ true)?;
            Ok(Outcome::Attached { sha, subject })
        }
    }
}

fn sanitize_message(raw: &str) -> Result<String, CliError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(CliError::InvalidState(
            "note is empty, nothing attached.".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

fn notes_ref_arg() -> String {
    format!("--ref={STACK_NOTES_REF}")
}

fn attach_note(
    repo_dir: Option<&Path>,
    sha: &str,
    message: &str,
    append: bool,
) -> Result<(), CliError> {
    let notes_ref = notes_ref_arg();
    let verb = if append { "append" } else { "add" };
    let mut argv: Vec<&str> = vec!["notes", &notes_ref, verb];
    // Python passes `-f` unconditionally on the set path so the
    // `add` command replaces any existing note instead of erroring.
    if !append {
        argv.push("-f");
    }
    argv.extend_from_slice(&["-m", message, sha]);
    run_git(repo_dir, &argv).map(|_| ())
}

fn note_exists(repo_dir: Option<&Path>, sha: &str) -> Result<bool, CliError> {
    let notes_ref = notes_ref_arg();
    let mut cmd = Command::new("git");
    if let Some(dir) = repo_dir {
        cmd.arg("-C").arg(dir);
    }
    cmd.args(["notes", &notes_ref, "show", sha]);
    let output = cmd
        .output()
        .map_err(|e| CliError::Generic(format!("failed to spawn `git notes show`: {e}")))?;
    Ok(output.status.success())
}

fn resolve_commit(
    repo_dir: Option<&Path>,
    commit: Option<&str>,
) -> Result<(String, String), CliError> {
    let sha = match commit {
        None => run_git(repo_dir, &["rev-parse", "--verify", "HEAD^{commit}"])?,
        Some(value) if change_id::is_prefix(value) => resolve_change_id_prefix(repo_dir, value)?,
        Some(value) => {
            let spec = format!("{value}^{{commit}}");
            run_git(repo_dir, &["rev-parse", "--verify", &spec])?
        }
    };
    let subject = run_git(repo_dir, &["log", "-1", "--format=%s", &sha])?;
    Ok((sha, subject))
}

fn resolve_change_id_prefix(repo_dir: Option<&Path>, prefix: &str) -> Result<String, CliError> {
    let resolved_repo = match repo_dir {
        Some(dir) => dir.to_path_buf(),
        None => resolve_repo_toplevel(None)?,
    };
    let trunk = trunk::get_trunk(Some(&resolved_repo))
        .map_err(|e| CliError::Generic(format!("couldn't resolve trunk to walk the stack: {e}")))?;
    let base = run_git(
        Some(&resolved_repo),
        &["merge-base", &trunk.refspec(), "HEAD"],
    )?;
    let commits = local_commits::read(&resolved_repo, &base, "HEAD")?;
    let matches: Vec<&local_commits::LocalCommit> = commits
        .iter()
        .filter(|c| c.change_id.starts_with(prefix))
        .collect();
    match matches.as_slice() {
        [] => Err(CliError::InvalidState(format!(
            "no commit found matching Change-Id prefix '{prefix}'"
        ))),
        [only] => Ok(only.commit_sha.clone()),
        many => {
            let listing = many
                .iter()
                .map(|c| format!("{} {}", &c.commit_sha[..7], c.title))
                .collect::<Vec<_>>()
                .join("\n  ");
            Err(CliError::InvalidState(format!(
                "Change-Id prefix '{prefix}' matches multiple commits:\n  {listing}"
            )))
        }
    }
}

fn resolve_repo_toplevel(repo_dir: Option<&Path>) -> Result<PathBuf, CliError> {
    let raw = run_git(repo_dir, &["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(raw))
}

/// Open the user's editor on a tempfile pre-filled with the
/// comment template, then read it back and strip comment lines.
/// Mirrors Python's `_read_note_from_editor`.
fn read_note_from_editor() -> Result<String, CliError> {
    // Treat empty env-var values as unset so `GIT_EDITOR=` falls
    // through to `$VISUAL` / `$EDITOR` / `vi` instead of spawning
    // an empty command. Matches Python's `or`-chain semantics.
    let editor = non_empty_env("GIT_EDITOR")
        .or_else(|| non_empty_env("VISUAL"))
        .or_else(|| non_empty_env("EDITOR"))
        .unwrap_or_else(|| OsString::from("vi"));

    let mut tmp = tempfile::Builder::new()
        .prefix("mergify_note_")
        .suffix(".txt")
        .tempfile()
        .map_err(|e| CliError::Generic(format!("create editor tempfile: {e}")))?;
    tmp.write_all(EDITOR_TEMPLATE.as_bytes())
        .map_err(|e| CliError::Generic(format!("write editor template: {e}")))?;
    tmp.flush()
        .map_err(|e| CliError::Generic(format!("flush editor template: {e}")))?;
    let path = tmp.into_temp_path();

    // Python uses `shlex.split(editor)` then exec — accept editor
    // strings that include args ("code -w" etc.). Approximate via
    // `/bin/sh -c "<editor> <path>"` on Unix, and `cmd /c …` on
    // Windows; this is what `git` itself does to honor compound
    // editor strings.
    let status = invoke_editor(&editor, path.to_string_lossy().as_ref())?;
    if !status.success() {
        return Err(CliError::Generic(format!(
            "editor {editor:?} exited with status {status:?}"
        )));
    }

    let mut buf = String::new();
    std::fs::File::open(&path)
        .and_then(|mut f| f.read_to_string(&mut buf))
        .map_err(|e| CliError::Generic(format!("read editor tempfile: {e}")))?;

    // Drop comment lines, mirror Python.
    let cleaned: String = buf
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        return Err(CliError::InvalidState(
            "note is empty, nothing attached.".to_string(),
        ));
    }
    Ok(cleaned)
}

/// Read an env var, returning `None` for both unset *and* empty.
/// `OsString::is_empty` covers both `KEY` being absent and
/// `KEY=` exporting an empty string (which Python's `or` chain
/// in `_read_note_from_editor` also treats as unset).
fn non_empty_env(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|v| !v.is_empty())
}

#[cfg(unix)]
fn invoke_editor(editor: &OsString, path: &str) -> Result<std::process::ExitStatus, CliError> {
    let cmd_line = format!("{} \"$@\"", editor.to_string_lossy());
    Command::new("sh")
        .arg("-c")
        .arg(&cmd_line)
        .arg("sh")
        .arg(path)
        .status()
        .map_err(|e| CliError::Generic(format!("spawn editor: {e}")))
}

#[cfg(windows)]
fn invoke_editor(editor: &OsString, path: &str) -> Result<std::process::ExitStatus, CliError> {
    // Tempfile paths commonly contain spaces (e.g. `C:\Users\Foo
    // Bar\AppData\Local\Temp\…`), so we cannot just concatenate
    // the editor string and the path verbatim — cmd.exe would
    // split on the space and hand the editor a truncated path.
    // Wrap the path in `""` so cmd treats it as a single token;
    // the editor itself sees the unquoted path through argv.
    let cmd_line = format!("{} \"{}\"", editor.to_string_lossy(), path);
    Command::new("cmd")
        .args(["/C", &cmd_line])
        .status()
        .map_err(|e| CliError::Generic(format!("spawn editor: {e}")))
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
    use tempfile::TempDir;

    fn init_repo() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@e.com"],
            &["config", "user.name", "T"],
            &["commit", "--allow-empty", "-m", "root"],
        ] {
            run_in(dir.path(), args);
        }
        dir
    }

    fn run_in(dir: &Path, args: &[&str]) {
        let ok = crate::test_env::isolated_git()
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git -C {dir:?} {args:?} failed");
    }

    fn read_note(dir: &Path, sha: &str) -> Option<String> {
        let out = crate::test_env::isolated_git()
            .arg("-C")
            .arg(dir)
            .args(["notes", "--ref=refs/notes/mergify/stack", "show", sha])
            .output()
            .unwrap();
        if out.status.success() {
            Some(String::from_utf8(out.stdout).unwrap().trim().to_string())
        } else {
            None
        }
    }

    fn head_sha(dir: &Path) -> String {
        let out = crate::test_env::isolated_git()
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[cfg(unix)]
    fn set_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn add_attaches_note_to_head() {
        let dir = init_repo();
        let outcome = run(Some(dir.path()), None, Action::Set("fixed a typo".into())).unwrap();
        assert!(matches!(outcome, Outcome::Attached { .. }));
        assert_eq!(
            read_note(dir.path(), &head_sha(dir.path())).as_deref(),
            Some("fixed a typo")
        );
    }

    #[test]
    fn add_to_specific_sha_via_prefix() {
        let dir = init_repo();
        let first = head_sha(dir.path());
        run_in(dir.path(), &["commit", "--allow-empty", "-m", "second"]);

        let outcome = run(
            Some(dir.path()),
            Some(&first[..10]),
            Action::Set("note for first".into()),
        )
        .unwrap();
        assert!(matches!(outcome, Outcome::Attached { .. }));
        assert_eq!(
            read_note(dir.path(), &first).as_deref(),
            Some("note for first")
        );
    }

    #[test]
    fn append_concatenates_with_blank_line() {
        let dir = init_repo();
        run(Some(dir.path()), None, Action::Set("first line".into())).unwrap();
        run(Some(dir.path()), None, Action::Append("second line".into())).unwrap();
        assert_eq!(
            read_note(dir.path(), &head_sha(dir.path())).as_deref(),
            Some("first line\n\nsecond line")
        );
    }

    #[test]
    fn replace_is_default() {
        let dir = init_repo();
        run(Some(dir.path()), None, Action::Set("first".into())).unwrap();
        run(Some(dir.path()), None, Action::Set("second".into())).unwrap();
        assert_eq!(
            read_note(dir.path(), &head_sha(dir.path())).as_deref(),
            Some("second")
        );
    }

    #[test]
    fn remove_deletes_note() {
        let dir = init_repo();
        run(Some(dir.path()), None, Action::Set("doomed".into())).unwrap();
        let outcome = run(Some(dir.path()), None, Action::Remove).unwrap();
        assert!(matches!(outcome, Outcome::Removed { .. }));
        assert!(read_note(dir.path(), &head_sha(dir.path())).is_none());
    }

    #[test]
    fn remove_is_idempotent() {
        let dir = init_repo();
        let outcome = run(Some(dir.path()), None, Action::Remove).unwrap();
        assert!(matches!(outcome, Outcome::NoNoteToRemove { .. }));
    }

    #[test]
    fn empty_inline_message_is_rejected() {
        let dir = init_repo();
        let err = run(Some(dir.path()), None, Action::Set("   ".into())).unwrap_err();
        match err {
            CliError::InvalidState(msg) => {
                assert!(msg.contains("note is empty"), "got: {msg}");
            }
            other => panic!("unexpected: {other:?}"),
        }
        // No note was written.
        assert!(read_note(dir.path(), &head_sha(dir.path())).is_none());
    }

    /// Editor-fallback parity regression. The Python code stripped
    /// comment lines and trimmed the surrounding whitespace before
    /// writing the note; we mirror that here so a user's
    /// `# Why was this commit amended? …` template line never
    /// leaks into the stored note.
    #[cfg(unix)]
    #[test]
    fn editor_fallback_strips_comments_and_writes_real_note() {
        let dir = init_repo();
        let editor = dir.path().join("fake-editor.sh");
        std::fs::write(
            &editor,
            "#!/bin/sh\nprintf 'real note text\\n# template line\\n' > \"$1\"\n",
        )
        .unwrap();
        set_executable(&editor);

        temp_env::with_var("GIT_EDITOR", Some(editor.to_str().unwrap()), || {
            run(Some(dir.path()), None, Action::FromEditor).unwrap();
        });
        assert_eq!(
            read_note(dir.path(), &head_sha(dir.path())).as_deref(),
            Some("real note text"),
        );
    }

    /// Empty editor output (comments only, no real content) is
    /// rejected as `InvalidState` — the user accidentally wrote
    /// nothing, don't silently attach a blank note. Matches
    /// Python's `sys.exit(ExitCode.INVALID_STATE)`.
    #[cfg(unix)]
    #[test]
    fn editor_fallback_rejects_empty_result() {
        let dir = init_repo();
        let editor = dir.path().join("empty-editor.sh");
        std::fs::write(
            &editor,
            "#!/bin/sh\nprintf '# only a comment\\n   \\n' > \"$1\"\n",
        )
        .unwrap();
        set_executable(&editor);

        let err = temp_env::with_var("GIT_EDITOR", Some(editor.to_str().unwrap()), || {
            run(Some(dir.path()), None, Action::FromEditor).unwrap_err()
        });
        match err {
            CliError::InvalidState(msg) => assert!(msg.contains("note is empty"), "got: {msg}"),
            other => panic!("unexpected: {other:?}"),
        }
        assert!(read_note(dir.path(), &head_sha(dir.path())).is_none());
    }

    /// `GIT_EDITOR=` (empty) must fall through to `$VISUAL` —
    /// Python's `or` chain treats empty strings as unset. Without
    /// this filter the spawn would fail trying to execute "".
    #[cfg(unix)]
    #[test]
    fn editor_fallback_treats_empty_env_var_as_unset() {
        let dir = init_repo();
        let editor = dir.path().join("visual-editor.sh");
        std::fs::write(&editor, "#!/bin/sh\nprintf 'from VISUAL\\n' > \"$1\"\n").unwrap();
        set_executable(&editor);

        temp_env::with_vars(
            [
                ("GIT_EDITOR", Some(String::new())),
                ("VISUAL", Some(editor.to_str().unwrap().to_string())),
            ],
            || {
                run(Some(dir.path()), None, Action::FromEditor).unwrap();
            },
        );
        assert_eq!(
            read_note(dir.path(), &head_sha(dir.path())).as_deref(),
            Some("from VISUAL"),
        );
    }

    #[test]
    fn change_id_prefix_resolves_against_stack() {
        // Set up: bare upstream + local clone with one commit on
        // `feature` carrying a Change-Id trailer.
        let workdir = tempfile::tempdir().unwrap();
        let upstream = workdir.path().join("up.git");
        let local = workdir.path().join("local");
        let ok = crate::test_env::isolated_git()
            .args([
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                upstream.to_str().unwrap(),
            ])
            .status()
            .unwrap()
            .success();
        assert!(ok);

        std::fs::create_dir(&local).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@e.com"],
            &["config", "user.name", "T"],
            &["commit", "--allow-empty", "-m", "root"],
            &["remote", "add", "origin", upstream.to_str().unwrap()],
            &["push", "-q", "origin", "main"],
            &["remote", "set-head", "origin", "main"],
            &["checkout", "-q", "-b", "feature"],
        ] {
            run_in(&local, args);
        }
        // A commit that already carries a Change-Id trailer so the
        // local-commits walker accepts it.
        let change_id = "Iabcdef0123456789abcdef0123456789abcdef01";
        let msg = format!("feature\n\nChange-Id: {change_id}");
        run_in(&local, &["commit", "--allow-empty", "-m", &msg]);
        let target_sha = head_sha(&local);

        let outcome = run(
            Some(&local),
            Some(&change_id[..9]),
            Action::Set("by change-id".into()),
        )
        .unwrap();
        assert!(matches!(outcome, Outcome::Attached { .. }));
        assert_eq!(
            read_note(&local, &target_sha).as_deref(),
            Some("by change-id")
        );
    }

    #[test]
    fn change_id_prefix_with_no_match_errors() {
        // Repo with the right upstream wiring but no commit carrying
        // a matching Change-Id.
        let workdir = tempfile::tempdir().unwrap();
        let upstream = workdir.path().join("up.git");
        let local = workdir.path().join("local");
        crate::test_env::isolated_git()
            .args([
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                upstream.to_str().unwrap(),
            ])
            .status()
            .unwrap();
        std::fs::create_dir(&local).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@e.com"],
            &["config", "user.name", "T"],
            &["commit", "--allow-empty", "-m", "root"],
            &["remote", "add", "origin", upstream.to_str().unwrap()],
            &["push", "-q", "origin", "main"],
            &["remote", "set-head", "origin", "main"],
            &["checkout", "-q", "-b", "feature"],
        ] {
            run_in(&local, args);
        }
        let msg = "feature\n\nChange-Id: Idead0000000000000000000000000000000000ff";
        run_in(&local, &["commit", "--allow-empty", "-m", msg]);

        let err = run(
            Some(&local),
            Some("Ibeef"),
            Action::Set("never lands".into()),
        )
        .unwrap_err();
        match err {
            CliError::InvalidState(m) => {
                assert!(m.contains("no commit found matching Change-Id"), "got: {m}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
