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
//! one command at a time. Phase 6 deletes this crate entirely.

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

#[derive(thiserror::Error, Debug)]
pub enum ShimError {
    #[error(
        "could not locate a Python interpreter. Expected `{expected}` (sibling of the mergify \
         binary) or `${env_var}` to be set to a python3.13+ executable"
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

    // Layouts to probe, in order:
    // 1. `<exe-dir>/python(.exe)` — Linux/macOS pip & pipx, Windows
    //    venv (everything ends up under `Scripts/`).
    // 2. `<exe-dir>/../python.exe` — Windows system-Python pip
    //    install: the binary lands in `<prefix>/Scripts/` while the
    //    interpreter sits at `<prefix>/python.exe`.
    let candidates = python_candidate_paths(parent);

    for candidate in &candidates {
        if candidate.is_file() {
            return Ok(candidate.clone());
        }
    }

    Err(ShimError::PythonNotFound {
        expected: candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", "),
        env_var: PYTHON_EXE_ENV,
    })
}

#[cfg(not(windows))]
fn python_candidate_paths(parent: &std::path::Path) -> Vec<PathBuf> {
    vec![parent.join(PYTHON_FILENAME)]
}

#[cfg(windows)]
fn python_candidate_paths(parent: &std::path::Path) -> Vec<PathBuf> {
    let mut candidates = vec![parent.join(PYTHON_FILENAME)];
    if let Some(grandparent) = parent.parent() {
        candidates.push(grandparent.join(PYTHON_FILENAME));
    }
    candidates
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
}
