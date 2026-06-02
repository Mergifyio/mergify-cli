//! Python shim for the mergify CLI Rust port.
//!
//! The Rust binary (`mergify`) is shipped inside a maturin-built
//! Python wheel. When the wheel is installed (`pipx install
//! mergify-cli`) the binary lands at `<venv>/bin/mergify` and the
//! Python source at `<venv>/lib/pythonX.Y/site-packages/mergify_cli/`.
//! For un-ported subcommands the shim locates the venv's `python3`
//! (sibling of the binary) and invokes `python3 -m mergify_cli` with
//! the original argv.
//!
//! Args, stdin, stdout, stderr, and the exit code pass through
//! transparently.
//!
//! Discovery: by default we resolve `<current_exe>/../python3` (or
//! `python.exe` on Windows). When the binary is run from a
//! `cargo build` checkout (no sibling Python), the
//! `MERGIFY_PYTHON_EXE` env var overrides — point it at any
//! interpreter that has the package available on `sys.path`.
//!
//! When a command ships native in Rust the caller dispatches to the
//! native impl first and only falls back to [`run`] for un-ported
//! commands. The plan is for each port PR to delete its Python
//! implementation in the same change, so the shim's reach shrinks
//! one command at a time; this crate is deleted entirely once
//! everything is ported.

use std::env;
use std::io;
use std::path::PathBuf;
use std::process::Command;

#[cfg(windows)]
const PYTHON_FILENAME: &str = "python.exe";
#[cfg(not(windows))]
const PYTHON_FILENAME: &str = "python3";

/// Env override for the Python interpreter. Useful in `cargo build`
/// checkouts where the binary has no sibling Python in the same
/// `bin/` directory (developer convenience).
const PYTHON_EXE_ENV: &str = "MERGIFY_PYTHON_EXE";

/// PyPI-normalized distribution name. `uv tool install mergify-cli`
/// creates the venv at `<uv-tool-dir>/<this>/Scripts/python.exe`.
/// Older uv versions stored the underscore form (`mergify_cli`);
/// we probe both for safety. Hardcoded to our package name — this
/// is intentional, the shim only services this one tool.
#[cfg(windows)]
const UV_TOOL_DIST_NAMES: &[&str] = &["mergify-cli", "mergify_cli"];

#[derive(thiserror::Error, Debug)]
pub enum ShimError {
    #[error(
        "could not locate a Python interpreter (tried: {expected}). Ensure `${env_var}` points \
         to a python3.13+ executable. If you installed via `uv tool install mergify-cli` on \
         Windows and `uv` is not on PATH, set `${env_var}` to \
         `<uv tool dir>\\mergify-cli\\Scripts\\python.exe`."
    )]
    PythonNotFound {
        expected: String,
        env_var: &'static str,
    },

    #[error("could not determine the path of the running mergify binary: {0}")]
    SelfPathUnknown(#[source] io::Error),

    #[error("could not invoke python interpreter at {path}: {source}")]
    Invocation {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Run the bundled Python CLI with the given argv tail.
///
/// Returns the exit code to propagate to the OS.
pub fn run(args: &[String]) -> Result<i32, ShimError> {
    let python = locate_python()?;
    invoke(&python, args)
}

fn locate_python() -> Result<PathBuf, ShimError> {
    if let Ok(explicit) = env::var(PYTHON_EXE_ENV) {
        if !explicit.is_empty() {
            let path = PathBuf::from(&explicit);
            // Validate eagerly: if `MERGIFY_PYTHON_EXE` is set but
            // points at a missing/non-file path, surface that here
            // with the exact value the user provided, rather than
            // letting `Command::new(...).status()` later fail with
            // an opaque `No such file or directory` error.
            if !path.is_file() {
                return Err(ShimError::PythonNotFound {
                    expected: explicit,
                    env_var: PYTHON_EXE_ENV,
                });
            }
            return Ok(path);
        }
    }

    let exe = env::current_exe().map_err(ShimError::SelfPathUnknown)?;
    locate_python_for_exe(&exe)
}

fn locate_python_for_exe(exe: &std::path::Path) -> Result<PathBuf, ShimError> {
    locate_python_for_exe_with(exe, default_uv_tool_dir_query)
}

/// Production hook for the uv-tool-dir lookup. Wraps
/// [`query_uv_tool_dir`] on Windows; no-op everywhere else.
/// Exists so the test-only `locate_python_for_exe_with` can pass
/// a stub that asserts whether the production code reached for
/// `uv` — see the laziness test.
fn default_uv_tool_dir_query() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        query_uv_tool_dir()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn locate_python_for_exe_with<F>(exe: &std::path::Path, query_uv: F) -> Result<PathBuf, ShimError>
where
    F: FnOnce() -> Option<PathBuf>,
{
    // `env::current_exe()` on macOS (and Windows) returns the path
    // used to invoke the binary, *without* resolving symlinks.
    // pipx installs the wheel's `mergify` binary at
    // `<venv>/bin/mergify` and exposes it via a `~/.local/bin/mergify`
    // symlink; if we don't follow that symlink, we end up looking for
    // python next to the user-facing `~/.local/bin/`, where there
    // typically is none, and surface "could not locate a Python
    // interpreter" on every invocation. Canonicalize first; fall
    // back to the original path if canonicalization fails (for
    // example if the binary was deleted between exec and now).
    let resolved = exe.canonicalize().unwrap_or_else(|_| exe.to_path_buf());
    let parent = resolved.parent().ok_or_else(|| {
        ShimError::SelfPathUnknown(io::Error::new(
            io::ErrorKind::NotFound,
            "current_exe has no parent directory",
        ))
    })?;

    // Phase 1 — in-tree probes. Cover every install layout that
    // doesn't need an out-of-process lookup:
    //
    // 1. `<exe-dir>/python(.exe)` — Linux/macOS pip & pipx, Windows
    //    venv (everything ends up under `Scripts/`).
    // 2. `<exe-dir>/../python.exe` — Windows system-Python pip
    //    install: the binary lands in `<prefix>/Scripts/` while the
    //    interpreter sits at `<prefix>/python.exe`.
    let in_tree = in_tree_python_candidates(parent);
    if let Some(found) = first_existing_file(&in_tree) {
        return Ok(found);
    }

    // Phase 2 (Windows only) — uv tool install layout. `uv tool
    // install mergify-cli` copies a standalone trampoline `.exe`
    // into the uv tool *bin* dir while the real venv lives at
    // `<uv tool dir>/mergify-cli/Scripts/python.exe`, a completely
    // separate tree. `canonicalize()` is a no-op against a
    // standalone .exe, so neither in-tree probe reaches it; we
    // have to ask uv where its tool dir is. Lazy on purpose —
    // spawning a subprocess on every shim invocation when the
    // in-tree probes would have succeeded is a regression.
    #[cfg(windows)]
    {
        if let Some(uv_tool_dir) = query_uv() {
            let uv_candidates = uv_tool_python_candidates(&uv_tool_dir);
            if let Some(found) = first_existing_file(&uv_candidates) {
                return Ok(found);
            }
            return Err(ShimError::PythonNotFound {
                expected: format_candidates(in_tree.iter().chain(uv_candidates.iter())),
                env_var: PYTHON_EXE_ENV,
            });
        }
    }
    #[cfg(not(windows))]
    let _ = query_uv;

    Err(ShimError::PythonNotFound {
        expected: format_candidates(in_tree.iter()),
        env_var: PYTHON_EXE_ENV,
    })
}

fn first_existing_file(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|p| p.is_file()).cloned()
}

fn format_candidates<'a, I: IntoIterator<Item = &'a PathBuf>>(candidates: I) -> String {
    candidates
        .into_iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(not(windows))]
fn in_tree_python_candidates(parent: &std::path::Path) -> Vec<PathBuf> {
    vec![parent.join(PYTHON_FILENAME)]
}

#[cfg(windows)]
fn in_tree_python_candidates(parent: &std::path::Path) -> Vec<PathBuf> {
    let mut candidates = vec![parent.join(PYTHON_FILENAME)];
    if let Some(grandparent) = parent.parent() {
        candidates.push(grandparent.join(PYTHON_FILENAME));
    }
    candidates
}

/// Per-tool venv layout `uv tool install` produces. Both PyPI-
/// normalized and underscore forms appear because older uv
/// versions didn't normalize the distribution name; first hit
/// wins in the lookup loop.
#[cfg(windows)]
fn uv_tool_python_candidates(uv_tool_dir: &std::path::Path) -> Vec<PathBuf> {
    UV_TOOL_DIST_NAMES
        .iter()
        .map(|name| uv_tool_dir.join(name).join("Scripts").join(PYTHON_FILENAME))
        .collect()
}

/// Spawn `uv tool dir` and return its stdout as a `PathBuf`. Used
/// only on Windows, only when the in-tree probes miss. Returns
/// `None` whenever `uv` is unreachable, exits non-zero, or prints
/// nothing — callers fall through to the existing
/// `PythonNotFound` error.
#[cfg(windows)]
fn query_uv_tool_dir() -> Option<PathBuf> {
    let output = Command::new("uv").args(["tool", "dir"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let path = std::str::from_utf8(&output.stdout).ok()?.trim();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

fn invoke(python: &std::path::Path, args: &[String]) -> Result<i32, ShimError> {
    let mut cmd = Command::new(python);
    cmd.arg("-m")
        .arg("mergify_cli")
        .args(args)
        // PYTHONSAFEPATH=1 stops Python from prepending the current
        // working directory to sys.path. Without it, a user running
        // from a directory that happens to contain a `mergify_cli/`
        // folder would import that instead of the wheel's copy.
        // Safe since the project requires Python 3.13+ and
        // PYTHONSAFEPATH was introduced in 3.11.
        .env("PYTHONSAFEPATH", "1");
    // On Windows, force `PYTHONUTF8=1`. The Python `main()` has a
    // legacy re-exec block (`subprocess.Popen(sys.argv, ...)`) that
    // re-launches itself with utf8 mode when not already on. That
    // re-exec assumes `sys.argv[0]` is a launcher binary; under
    // `python -m mergify_cli` it's a `.py` file path, which Windows
    // can't directly exec — `OSError [WinError 193] %1 is not a
    // valid Win32 application`. Booting Python in utf8 mode skips
    // the re-exec entirely.
    #[cfg(windows)]
    cmd.env("PYTHONUTF8", "1");
    let status = cmd.status().map_err(|source| ShimError::Invocation {
        path: python.to_path_buf(),
        source,
    })?;

    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locate_python_honors_env_override_when_file_exists() {
        // Use the test binary itself as a stand-in for python — it
        // exists and is a regular file, which is all `locate_python`
        // checks (executability is enforced by `Command::new`).
        let test_binary = env::current_exe().unwrap();
        let path_str = test_binary.to_str().unwrap();
        temp_env::with_var(PYTHON_EXE_ENV, Some(path_str), || {
            let got = locate_python().unwrap();
            assert_eq!(got, test_binary);
        });
    }

    #[test]
    fn locate_python_rejects_env_override_pointing_at_missing_file() {
        temp_env::with_var(PYTHON_EXE_ENV, Some("/nonexistent/python"), || {
            let err = locate_python().unwrap_err();
            let msg = err.to_string();
            assert!(matches!(err, ShimError::PythonNotFound { .. }));
            // The user-supplied path must appear in the error so
            // they can spot a typo without having to dig.
            assert!(msg.contains("/nonexistent/python"), "got: {msg}");
        });
    }

    #[cfg(unix)]
    #[test]
    fn locate_python_for_exe_follows_symlinks() {
        // Regression for INC-1352 / MRGFY-7173: `env::current_exe()`
        // on macOS returns the symlink path, not its target. pipx
        // exposes the wheel binary as `~/.local/bin/mergify` ->
        // `<venv>/bin/mergify`; the python interpreter lives next
        // to the canonical target, not the symlink. The shim must
        // canonicalize before probing for a sibling python.
        let real_dir = tempfile::tempdir().unwrap();
        let link_dir = tempfile::tempdir().unwrap();

        let real_bin = real_dir.path().join("mergify");
        let real_python = real_dir.path().join(PYTHON_FILENAME);
        std::fs::write(&real_bin, b"").unwrap();
        std::fs::write(&real_python, b"").unwrap();

        let symlinked_bin = link_dir.path().join("mergify");
        std::os::unix::fs::symlink(&real_bin, &symlinked_bin).unwrap();

        let got = locate_python_for_exe(&symlinked_bin).unwrap();
        assert_eq!(
            got.canonicalize().unwrap(),
            real_python.canonicalize().unwrap()
        );
    }

    #[test]
    fn locate_python_errors_when_no_sibling_and_no_env() {
        // The test binary lives under `target/debug/deps/<name>`;
        // there's no python3 next to it. So the lookup must fail
        // with a clear `PythonNotFound` describing where we looked.
        temp_env::with_var_unset(PYTHON_EXE_ENV, || {
            let err = locate_python().unwrap_err();
            assert!(matches!(err, ShimError::PythonNotFound { .. }));
        });
    }

    #[test]
    fn python_not_found_error_mentions_uv_tool_install_workaround_for_windows_users() {
        // Reporters who hit this on Windows + `uv tool install` need
        // to learn about MERGIFY_PYTHON_EXE without filing a ticket.
        // Pin the actionable parts of the message so a rewrite that
        // accidentally drops them surfaces here.
        let err = ShimError::PythonNotFound {
            expected: "C:\\bogus\\python.exe".to_string(),
            env_var: PYTHON_EXE_ENV,
        };
        let msg = err.to_string();
        assert!(msg.contains(PYTHON_EXE_ENV), "msg: {msg}");
        assert!(msg.contains("uv tool install mergify-cli"), "msg: {msg}");
        // The candidate list the discovery actually tried must
        // appear so users can rule out typos / wrong working dirs.
        assert!(msg.contains("C:\\bogus\\python.exe"), "msg: {msg}");
    }

    #[cfg(windows)]
    #[test]
    fn uv_tool_candidate_list_includes_both_normalized_and_underscore_distribution_names() {
        // Both PyPI-normalized (`mergify-cli`) and legacy
        // underscore (`mergify_cli`) forms must appear so the
        // lookup works regardless of which uv release laid the
        // tool down — first hit wins.
        let uv_tool_dir = std::path::PathBuf::from("D:\\uv-tools");
        let candidates = uv_tool_python_candidates(&uv_tool_dir);
        let display: Vec<String> = candidates.iter().map(|p| p.display().to_string()).collect();
        assert!(
            display
                .iter()
                .any(|p| p.contains("uv-tools\\mergify-cli\\Scripts\\python.exe")),
            "candidates missing PyPI-normalized uv layout: {display:?}",
        );
        assert!(
            display
                .iter()
                .any(|p| p.contains("uv-tools\\mergify_cli\\Scripts\\python.exe")),
            "candidates missing underscore-form uv layout: {display:?}",
        );
    }

    #[cfg(windows)]
    #[test]
    fn uv_tool_dir_query_does_not_fire_when_sibling_python_exists() {
        // Lazy fallback: the `uv tool dir` subprocess is only
        // worth its cost when the cheap in-tree probes have all
        // missed. A regression here makes every Windows shim run
        // pay a subprocess spawn — exactly what Copilot caught on
        // the first revision of this fix. Drive the discovery
        // with a tempdir that DOES have a sibling python and a
        // query stub that flips a flag if reached.
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("mergify.exe");
        let python = dir.path().join(PYTHON_FILENAME);
        std::fs::write(&bin, b"").unwrap();
        std::fs::write(&python, b"").unwrap();

        let queried = AtomicBool::new(false);
        let got = locate_python_for_exe_with(&bin, || {
            queried.store(true, Ordering::SeqCst);
            None
        })
        .unwrap();

        assert_eq!(got.canonicalize().unwrap(), python.canonicalize().unwrap());
        assert!(
            !queried.load(Ordering::SeqCst),
            "`uv tool dir` query must not run when the sibling python is present",
        );
    }

    #[cfg(windows)]
    #[test]
    fn uv_tool_dir_query_fires_and_succeeds_when_no_in_tree_python_exists() {
        // Counterpart to the laziness test: when the in-tree
        // probes miss, the query stub IS reached and the matching
        // venv python is preferred over erroring.
        let uv_dir = tempfile::tempdir().unwrap();
        let scripts = uv_dir.path().join("mergify-cli").join("Scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        let python = scripts.join(PYTHON_FILENAME);
        std::fs::write(&python, b"").unwrap();

        let bin_dir = tempfile::tempdir().unwrap();
        let bin = bin_dir.path().join("mergify.exe");
        std::fs::write(&bin, b"").unwrap();

        let got = locate_python_for_exe_with(&bin, || Some(uv_dir.path().to_path_buf())).unwrap();
        assert_eq!(got.canonicalize().unwrap(), python.canonicalize().unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn error_lists_uv_candidates_when_uv_tool_dir_is_known_but_no_python_lives_there() {
        // If `uv tool dir` answers but the per-tool venv has no
        // `python.exe`, the error message must still name what we
        // tried so the user can spot a half-installed tool dir.
        let uv_dir = tempfile::tempdir().unwrap();
        let bin_dir = tempfile::tempdir().unwrap();
        let bin = bin_dir.path().join("mergify.exe");
        std::fs::write(&bin, b"").unwrap();

        let err =
            locate_python_for_exe_with(&bin, || Some(uv_dir.path().to_path_buf())).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("mergify-cli\\Scripts\\python.exe"),
            "msg: {msg}"
        );
        assert!(
            msg.contains("mergify_cli\\Scripts\\python.exe"),
            "msg: {msg}"
        );
    }
}
