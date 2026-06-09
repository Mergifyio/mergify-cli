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
use std::process::Command;

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
pub fn install(opts: &Options<'_>) -> Result<Vec<InstallLog>, CliError> {
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

    ensure_notes_display_ref(opts.repo_dir)?;
    Ok(logs)
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

fn ensure_notes_display_ref(repo_dir: Option<&Path>) -> Result<(), CliError> {
    let desired = "refs/notes/mergify/*";
    let current = run_git_capture(
        repo_dir,
        &["config", "--local", "--get-all", "notes.displayRef"],
    )
    .unwrap_or_default();
    if current.lines().any(|l| l == desired) {
        return Ok(());
    }
    run_git_capture(
        repo_dir,
        &["config", "--local", "--add", "notes.displayRef", desired],
    )?;
    Ok(())
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
        let logs = install(&Options {
            repo_dir: Some(dir.path()),
            force: false,
        })
        .unwrap();
        assert_eq!(logs.len(), HOOK_NAMES.len());
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
        // installed wrappers across the board.
        for log in &second {
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

        let logs = install(&Options {
            repo_dir: Some(dir.path()),
            force: false,
        })
        .unwrap();
        let commit_msg = logs.iter().find(|l| l.hook_name == "commit-msg").unwrap();
        assert!(
            commit_msg
                .actions
                .iter()
                .any(|a| matches!(a, HookAction::WrapperLegacyNeedsForce))
        );

        // With --force, the legacy wrapper is migrated.
        let logs = install(&Options {
            repo_dir: Some(dir.path()),
            force: true,
        })
        .unwrap();
        let commit_msg = logs.iter().find(|l| l.hook_name == "commit-msg").unwrap();
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
}
