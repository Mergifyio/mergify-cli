//! `mergify stack hooks` (status + `--setup`) and `mergify stack
//! setup` — install the git hooks the stack workflow depends on.
//!
//! Port of `mergify_cli/stack/setup.py`. The on-disk layout is
//! the same one Python's installer wrote, so an existing checkout
//! upgraded from a Python install is recognised without
//! re-installation:
//!
//! ```text
//! .git/hooks/<hook>               # thin wrapper (sources the managed script;
//!                                 #  user may add custom logic below the marker)
//! .git/hooks/mergify-hooks/<hook>.sh  # managed script (always upgradable)
//! ```
//!
//! Wrappers carry a stable `mergify-hooks` + `<hook>.sh` substring
//! pair so the status detector can tell a Mergify wrapper from
//! the user's own hook. The legacy heuristics (commit-msg
//! `Change-Id: I${random}` and prepare-commit-msg
//! `is_amend_with_m_flag`) match the pre-sourcing-architecture
//! installs so `--force` knows to migrate them.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::git::run_git_capture;
use crate::local_commits::STACK_NOTES_REF;

use mergify_core::CliError;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// One git hook this crate manages. The names match files in
/// `crates/mergify-stack/hooks/{scripts,wrappers}/`.
const HOOK_NAMES: &[&str] = &[
    "commit-msg",
    "post-commit",
    "pre-push",
    "prepare-commit-msg",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum WrapperStatus {
    /// No wrapper present.
    Missing,
    /// Pre-sourcing-architecture wrapper — needs `--force` to migrate.
    Legacy,
    /// Our wrapper (sources `mergify-hooks/<hook>.sh`) OR the user
    /// installed their own hook in this slot. Both are treated
    /// the same: we don't touch them.
    Installed,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct HookStatus {
    pub hook_name: String,
    pub wrapper_status: WrapperStatus,
    pub script_installed: bool,
    pub script_needs_update: bool,
    pub wrapper_path: String,
    pub script_path: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct HooksStatus {
    pub git_hooks: Vec<HookStatus>,
}

/// What `_install_git_hook` did to each hook this run — surfaced
/// so the binary handler can render the same `Installing hook
/// wrapper: <name>` / `Updating managed hook script: …` / `Found
/// legacy hook: <name>` lines Python printed.
#[derive(Debug, Clone, serde::Serialize)]
pub enum HookAction {
    ScriptInstalled,
    ScriptUpdated,
    ScriptUpToDate,
    WrapperInstalled,
    WrapperMigrated,
    WrapperLegacyNeedsForce,
    WrapperAlreadyInstalled,
}

#[derive(Debug, Clone)]
pub struct InstallLog {
    pub hook_name: String,
    pub actions: Vec<HookAction>,
}

/// Outcome of an `install` run: the per-hook action log plus, for
/// each of the local `notes.displayRef` and `notes.rewriteRef`
/// configs, whether it was added this run (so the caller can echo
/// Python's `Added <key> = …` confirmation only for what changed).
#[derive(Debug, Clone)]
pub struct InstallOutcome {
    pub logs: Vec<InstallLog>,
    pub notes_display_ref_added: bool,
    pub notes_rewrite_ref_added: bool,
}

pub struct Options<'a> {
    pub repo_dir: Option<&'a Path>,
    pub force: bool,
}

/// Walk every managed hook and emit its current status. Mirrors
/// Python's `get_hooks_status`.
pub fn status(repo_dir: Option<&Path>) -> Result<HooksStatus, CliError> {
    let hooks_dir = resolve_hooks_dir(repo_dir)?;
    let managed_dir = hooks_dir.join("mergify-hooks");
    let mut git_hooks = Vec::with_capacity(HOOK_NAMES.len());
    for &hook in HOOK_NAMES {
        let wrapper_path = hooks_dir.join(hook);
        let script_path = managed_dir.join(format!("{hook}.sh"));
        let wrapper_status = wrapper_status(&wrapper_path, hook);
        let script_installed = script_path.exists();
        let script_needs_update = if script_installed {
            script_differs(&script_path, hook)
        } else {
            true
        };
        git_hooks.push(HookStatus {
            hook_name: hook.to_string(),
            wrapper_status,
            script_installed,
            script_needs_update,
            wrapper_path: wrapper_path.to_string_lossy().into_owned(),
            script_path: script_path.to_string_lossy().into_owned(),
        });
    }
    Ok(HooksStatus { git_hooks })
}

/// Install or upgrade every managed hook. Returns a per-hook
/// action log so callers can render the right messages.
pub fn install(opts: &Options<'_>) -> Result<InstallOutcome, CliError> {
    let hooks_dir = resolve_hooks_dir(opts.repo_dir)?;
    let managed_dir = hooks_dir.join("mergify-hooks");
    fs::create_dir_all(&managed_dir).map_err(|e| {
        CliError::Generic(format!(
            "create managed hooks dir {}: {e}",
            managed_dir.display()
        ))
    })?;

    let mut logs = Vec::with_capacity(HOOK_NAMES.len());
    for &hook in HOOK_NAMES {
        let mut actions = Vec::new();
        install_single_hook(&hooks_dir, &managed_dir, hook, opts.force, &mut actions)?;
        logs.push(InstallLog {
            hook_name: hook.to_string(),
            actions,
        });
    }

    let notes_display_ref_added = ensure_notes_display_ref(opts.repo_dir)?;
    let notes_rewrite_ref_added = ensure_notes_rewrite_ref(opts.repo_dir)?;
    Ok(InstallOutcome {
        logs,
        notes_display_ref_added,
        notes_rewrite_ref_added,
    })
}

fn install_single_hook(
    hooks_dir: &Path,
    managed_dir: &Path,
    hook: &str,
    force: bool,
    actions: &mut Vec<HookAction>,
) -> Result<(), CliError> {
    let script_path = managed_dir.join(format!("{hook}.sh"));
    let new_script = script_resource(hook);
    let wrapper_path = hooks_dir.join(hook);
    let new_wrapper = wrapper_resource(hook);

    // Managed script: install or refresh when the content drifts.
    if !script_path.exists() {
        write_file_with_mode(&script_path, new_script, 0o755)?;
        actions.push(HookAction::ScriptInstalled);
    } else if script_differs(&script_path, hook) {
        write_file_with_mode(&script_path, new_script, 0o755)?;
        actions.push(HookAction::ScriptUpdated);
    } else {
        actions.push(HookAction::ScriptUpToDate);
    }

    // Wrapper: per Python's status branches.
    match wrapper_status(&wrapper_path, hook) {
        WrapperStatus::Missing => {
            write_file_with_mode(&wrapper_path, new_wrapper, 0o755)?;
            actions.push(HookAction::WrapperInstalled);
        }
        WrapperStatus::Legacy => {
            if force {
                write_file_with_mode(&wrapper_path, new_wrapper, 0o755)?;
                actions.push(HookAction::WrapperMigrated);
            } else {
                actions.push(HookAction::WrapperLegacyNeedsForce);
            }
        }
        WrapperStatus::Installed => {
            actions.push(HookAction::WrapperAlreadyInstalled);
        }
    }
    Ok(())
}

/// Ensure `git log` surfaces mergify notes by adding the local
/// `notes.displayRef = refs/notes/mergify/*` config. Returns `true`
/// when the config was added this run, `false` when it was already
/// present.
fn ensure_notes_display_ref(repo_dir: Option<&Path>) -> Result<bool, CliError> {
    let desired = "refs/notes/mergify/*";
    let current = run_git_capture(
        repo_dir,
        &["config", "--local", "--get-all", "notes.displayRef"],
    )
    .unwrap_or_default();
    if current.lines().any(|l| l == desired) {
        return Ok(false);
    }
    run_git_capture(
        repo_dir,
        &["config", "--local", "--add", "notes.displayRef", desired],
    )?;
    Ok(true)
}

/// Ensure a rewritten commit keeps its stack note by adding the
/// local `notes.rewriteRef = refs/notes/mergify/stack` config.
/// Returns `true` when the config was added this run, `false` when
/// it was already present.
///
/// A note is addressed by commit SHA, so an amend or a rebase would
/// otherwise strand the reason on the pre-rewrite SHA and the push
/// that follows would record a blank `Reason`. This is the setting
/// that covers the rewrites git runs on its own — `git commit
/// --amend`, and the `git rebase --continue` that finishes a
/// conflicted rebase — which no flag on the CLI's own invocations
/// can reach.
fn ensure_notes_rewrite_ref(repo_dir: Option<&Path>) -> Result<bool, CliError> {
    let current = run_git_capture(
        repo_dir,
        &["config", "--local", "--get-all", "notes.rewriteRef"],
    )
    .unwrap_or_default();
    if current.lines().any(|l| l == STACK_NOTES_REF) {
        return Ok(false);
    }
    run_git_capture(
        repo_dir,
        &[
            "config",
            "--local",
            "--add",
            "notes.rewriteRef",
            STACK_NOTES_REF,
        ],
    )?;
    Ok(true)
}

fn wrapper_status(path: &Path, hook: &str) -> WrapperStatus {
    if !path.exists() {
        return WrapperStatus::Missing;
    }
    let Ok(content) = fs::read_to_string(path) else {
        return WrapperStatus::Missing;
    };
    let sentinel = format!("{hook}.sh");
    if content.contains("mergify-hooks") && content.contains(&sentinel) {
        return WrapperStatus::Installed;
    }
    if hook == "commit-msg" && content.contains("Change-Id: I${random}") {
        return WrapperStatus::Legacy;
    }
    if hook == "prepare-commit-msg" && content.contains("is_amend_with_m_flag") {
        return WrapperStatus::Legacy;
    }
    // User's own hook — leave it alone.
    WrapperStatus::Installed
}

fn script_differs(installed_path: &Path, hook: &str) -> bool {
    let Ok(installed) = fs::read_to_string(installed_path) else {
        return true;
    };
    installed != script_resource(hook)
}

fn script_resource(hook: &str) -> &'static str {
    match hook {
        "commit-msg" => include_str!("../../hooks/scripts/commit-msg.sh"),
        "post-commit" => include_str!("../../hooks/scripts/post-commit.sh"),
        "pre-push" => include_str!("../../hooks/scripts/pre-push.sh"),
        "prepare-commit-msg" => include_str!("../../hooks/scripts/prepare-commit-msg.sh"),
        other => panic!("unknown hook: {other}"),
    }
}

fn wrapper_resource(hook: &str) -> &'static str {
    match hook {
        "commit-msg" => include_str!("../../hooks/wrappers/commit-msg"),
        "post-commit" => include_str!("../../hooks/wrappers/post-commit"),
        "pre-push" => include_str!("../../hooks/wrappers/pre-push"),
        "prepare-commit-msg" => include_str!("../../hooks/wrappers/prepare-commit-msg"),
        other => panic!("unknown hook: {other}"),
    }
}

fn write_file_with_mode(path: &Path, content: &str, mode: u32) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| CliError::Generic(format!("create parent for {}: {e}", path.display())))?;
    }
    let mut f = fs::File::create(path)
        .map_err(|e| CliError::Generic(format!("create {}: {e}", path.display())))?;
    f.write_all(content.as_bytes())
        .map_err(|e| CliError::Generic(format!("write {}: {e}", path.display())))?;
    set_executable_bit(path, mode)?;
    Ok(())
}

#[cfg(unix)]
fn set_executable_bit(path: &Path, mode: u32) -> Result<(), CliError> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|e| CliError::Generic(format!("chmod {}: {e}", path.display())))
}

#[cfg(not(unix))]
fn set_executable_bit(_path: &Path, _mode: u32) -> Result<(), CliError> {
    // Windows: the executable bit doesn't exist; Git for Windows
    // honors the shebang on POSIX-style shell scripts regardless.
    Ok(())
}

fn resolve_hooks_dir(repo_dir: Option<&Path>) -> Result<PathBuf, CliError> {
    let raw = run_git_capture(repo_dir, &["rev-parse", "--git-path", "hooks"])?;
    let path = PathBuf::from(&raw);
    if path.is_absolute() {
        return Ok(path);
    }
    // `--git-path hooks` returns a path relative to either the
    // repo toplevel or the original cwd, depending on git's
    // version. Anchor to repo toplevel so callers don't need to
    // chdir.
    let toplevel = run_git_capture(repo_dir, &["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(toplevel).join(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    fn init_repo() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "t@e.com"],
            &["config", "user.name", "T"],
        ] {
            let ok = crate::test_env::isolated_git()
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok);
        }
        dir
    }

    #[test]
    fn status_reports_all_missing_in_fresh_repo() {
        let dir = init_repo();
        let s = status(Some(dir.path())).unwrap();
        assert_eq!(s.git_hooks.len(), HOOK_NAMES.len());
        for h in &s.git_hooks {
            assert_eq!(h.wrapper_status, WrapperStatus::Missing);
            assert!(!h.script_installed);
        }
    }

    #[test]
    fn install_writes_wrappers_and_scripts() {
        let dir = init_repo();
        let outcome = install(&Options {
            repo_dir: Some(dir.path()),
            force: false,
        })
        .unwrap();
        assert_eq!(outcome.logs.len(), HOOK_NAMES.len());
        assert!(outcome.notes_display_ref_added);
        let s = status(Some(dir.path())).unwrap();
        for h in &s.git_hooks {
            assert_eq!(h.wrapper_status, WrapperStatus::Installed);
            assert!(h.script_installed);
            assert!(!h.script_needs_update);
        }
    }

    #[test]
    fn install_is_idempotent() {
        let dir = init_repo();
        install(&Options {
            repo_dir: Some(dir.path()),
            force: false,
        })
        .unwrap();
        let second = install(&Options {
            repo_dir: Some(dir.path()),
            force: false,
        })
        .unwrap();
        // Second run should report up-to-date scripts and already-
        // installed wrappers across the board, and not re-add the
        // notes.displayRef config.
        assert!(!second.notes_display_ref_added);
        for log in &second.logs {
            assert!(log.actions.iter().any(|a| matches!(
                a,
                HookAction::ScriptUpToDate | HookAction::WrapperAlreadyInstalled
            )));
        }
    }

    #[test]
    fn legacy_wrapper_needs_force() {
        let dir = init_repo();
        // Plant a legacy commit-msg wrapper.
        let hooks_dir = resolve_hooks_dir(Some(dir.path())).unwrap();
        fs::create_dir_all(&hooks_dir).unwrap();
        fs::write(
            hooks_dir.join("commit-msg"),
            "#!/bin/sh\n# legacy\nrandom=$(date +%s)\necho \"Change-Id: I${random}\"\n",
        )
        .unwrap();

        let outcome = install(&Options {
            repo_dir: Some(dir.path()),
            force: false,
        })
        .unwrap();
        let commit_msg = outcome
            .logs
            .iter()
            .find(|l| l.hook_name == "commit-msg")
            .unwrap();
        assert!(
            commit_msg
                .actions
                .iter()
                .any(|a| matches!(a, HookAction::WrapperLegacyNeedsForce))
        );

        // With --force, the legacy wrapper is migrated.
        let outcome = install(&Options {
            repo_dir: Some(dir.path()),
            force: true,
        })
        .unwrap();
        let commit_msg = outcome
            .logs
            .iter()
            .find(|l| l.hook_name == "commit-msg")
            .unwrap();
        assert!(
            commit_msg
                .actions
                .iter()
                .any(|a| matches!(a, HookAction::WrapperMigrated))
        );
    }

    #[test]
    fn user_wrapper_is_left_alone() {
        let dir = init_repo();
        let hooks_dir = resolve_hooks_dir(Some(dir.path())).unwrap();
        fs::create_dir_all(&hooks_dir).unwrap();
        let user_content = "#!/bin/sh\necho 'my custom hook'\n";
        fs::write(hooks_dir.join("commit-msg"), user_content).unwrap();

        install(&Options {
            repo_dir: Some(dir.path()),
            force: false,
        })
        .unwrap();
        assert_eq!(
            fs::read_to_string(hooks_dir.join("commit-msg")).unwrap(),
            user_content
        );
    }

    #[test]
    fn install_adds_notes_display_ref() {
        let dir = init_repo();
        install(&Options {
            repo_dir: Some(dir.path()),
            force: false,
        })
        .unwrap();
        let out = StdCommand::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["config", "--local", "--get-all", "notes.displayRef"])
            .output()
            .unwrap();
        let content = String::from_utf8(out.stdout).unwrap();
        assert!(content.lines().any(|l| l == "refs/notes/mergify/*"));
    }

    /// Run `git <args>` in `dir` through the installed hooks,
    /// returning `(success, stderr)`.
    fn try_git(dir: &Path, args: &[&str]) -> (bool, String) {
        let out = crate::test_env::isolated_git()
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn git_stdout(dir: &Path, args: &[&str]) -> String {
        let out = crate::test_env::isolated_git()
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A repo stopped mid-rebase on a conflict, with the hooks
    /// installed. The trunk commit is back-dated: the amend guard
    /// reads a commit whose author date is at least two seconds old
    /// as an amend target, which is what a real trunk tip is.
    fn repo_stopped_at_conflict() -> TempDir {
        let dir = init_repo();
        let p = dir.path();
        install(&Options {
            repo_dir: Some(p),
            force: false,
        })
        .unwrap();

        std::fs::write(p.join("f"), "base\n").unwrap();
        assert!(try_git(p, &["add", "f"]).0);
        assert!(try_git(p, &["commit", "-q", "-m", "root"]).0);
        let trunk = git_stdout(p, &["rev-parse", "--abbrev-ref", "HEAD"]);
        assert!(try_git(p, &["checkout", "-q", "-b", "feature"]).0);
        std::fs::write(p.join("f"), "feature\n").unwrap();
        assert!(try_git(p, &["commit", "-q", "-a", "-m", "feat"]).0);
        assert!(try_git(p, &["checkout", "-q", &trunk]).0);
        std::fs::write(p.join("f"), "trunk\n").unwrap();
        assert!(
            try_git(
                p,
                &[
                    "commit",
                    "-q",
                    "-a",
                    "-m",
                    "trunk moves",
                    "--date=@1000000000 +0000",
                ],
            )
            .0
        );
        assert!(try_git(p, &["checkout", "-q", "feature"]).0);

        // Conflicts on `f`, leaving HEAD at the trunk tip.
        assert!(!try_git(p, &["rebase", &trunk]).0, "rebase must conflict");
        std::fs::write(p.join("f"), "resolved\n").unwrap();
        assert!(try_git(p, &["add", "f"]).0);
        dir
    }

    #[test]
    fn amend_at_a_rebase_conflict_is_refused() {
        // The trap the guard exists for: at a conflict HEAD is the
        // last commit the rebase applied — here the trunk tip — so an
        // amend rewrites *that* commit and re-maps the work to
        // another pull request's Change-Id.
        let dir = repo_stopped_at_conflict();
        let p = dir.path();
        let head_before = git_stdout(p, &["rev-parse", "HEAD"]);

        let (ok, stderr) = try_git(p, &["commit", "--amend", "--no-edit"]);
        assert!(!ok, "amend must be refused at a conflict pause");
        assert!(stderr.contains("Refusing to amend"), "got: {stderr}");
        assert_eq!(
            git_stdout(p, &["rev-parse", "HEAD"]),
            head_before,
            "the already-applied commit must be left alone"
        );

        // The documented override still gets through.
        assert!(
            try_git(p, &["commit", "--amend", "--no-edit", "--no-verify"]).0,
            "--no-verify overrides the guard"
        );
    }

    #[test]
    fn resolving_a_conflict_is_not_mistaken_for_an_amend() {
        // `git commit` at the same pause creates the resolved commit
        // rather than rewriting HEAD; the rebase then continues. The
        // guard must stay out of the way of both.
        let dir = repo_stopped_at_conflict();
        let p = dir.path();

        assert!(
            try_git(p, &["commit", "-q", "-m", "resolved"]).0,
            "resolving with a plain commit must be allowed"
        );
        assert!(try_git(p, &["rebase", "--continue"]).0);
        assert!(!p.join(".git/rebase-merge").exists());
    }

    #[test]
    fn amend_at_a_stack_edit_pause_is_allowed() {
        // `stack edit` pauses *on* the target commit with a clean
        // tree and records it in `rebase-merge/amend` — there the
        // amend is the documented way to continue.
        let dir = init_repo();
        let p = dir.path();
        install(&Options {
            repo_dir: Some(p),
            force: false,
        })
        .unwrap();
        for name in ["one", "two", "three"] {
            std::fs::write(p.join(name), format!("{name}\n")).unwrap();
            assert!(try_git(p, &["add", name]).0);
            assert!(try_git(p, &["commit", "-q", "-m", name, "--date=@1000000000 +0000"],).0);
        }

        let (ok, stderr) = try_git(
            p,
            &[
                "-c",
                "sequence.editor=sed -i.bak 1s/^pick/edit/",
                "rebase",
                "-i",
                "HEAD~2",
            ],
        );
        assert!(ok, "rebase must pause, not fail: {stderr}");
        assert!(p.join(".git/rebase-merge/amend").exists());

        std::fs::write(p.join("two"), "amended\n").unwrap();
        assert!(try_git(p, &["add", "two"]).0);
        let (ok, stderr) = try_git(p, &["commit", "--amend", "--no-edit"]);
        assert!(ok, "amend at an edit pause must be allowed: {stderr}");
        assert!(try_git(p, &["rebase", "--continue"]).0);
    }

    #[test]
    fn install_adds_notes_rewrite_ref_once() {
        let dir = init_repo();
        let opts = Options {
            repo_dir: Some(dir.path()),
            force: false,
        };
        assert!(install(&opts).unwrap().notes_rewrite_ref_added);
        // Second run finds it already there: no duplicate value, and
        // the outcome reports nothing added so the CLI stays quiet.
        assert!(!install(&opts).unwrap().notes_rewrite_ref_added);

        let out = StdCommand::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["config", "--local", "--get-all", "notes.rewriteRef"])
            .output()
            .unwrap();
        let content = String::from_utf8(out.stdout).unwrap();
        assert_eq!(
            content
                .lines()
                .filter(|l| *l == "refs/notes/mergify/stack")
                .count(),
            1
        );
    }
}
