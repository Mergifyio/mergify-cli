//! Python shim for the mergify CLI Rust port.
//!
//! The current Python source is embedded at compile time via
//! [`include_dir`]. On first invocation the shim extracts the
//! embedded tree to a per-user cache directory (atomic, file-locked)
//! and invokes `python3 -m mergify_cli` with that directory on
//! `PYTHONPATH`. Args, stdin, stdout, stderr, and the exit code are
//! passed through transparently.
//!
//! When a command is ported to native Rust in Phase 1.3+, the caller
//! dispatches to the native implementation first and falls back to
//! [`run`] only for un-ported commands. Phase 6 removes this crate
//! entirely once the port is complete.
//!
//! The cache is keyed on `CARGO_PKG_VERSION`. During dev (`0.0.0`),
//! that means the cache is shared across builds — if you change the
//! embedded Python source while developing, clear
//! `~/.cache/mergify/py/` to force a re-extract. The release
//! pipeline in Phase 1.5 stamps a real version + git SHA, after
//! which every build invalidates cleanly.

use std::env;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use fs2::FileExt;
use include_dir::Dir;
use include_dir::DirEntry;
use include_dir::include_dir;

static PY_SOURCE: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../../mergify_cli");

/// Cache key under `~/.cache/mergify/py/`. Tied to the binary's
/// build version so a new binary auto-invalidates any older extract.
const CACHE_KEY: &str = env!("CARGO_PKG_VERSION");

#[derive(thiserror::Error, Debug)]
pub enum ShimError {
    #[error(
        "python3 not found on PATH. mergify requires Python 3.13+ during the port; install it and try again"
    )]
    PythonNotFound,

    #[error("could not locate user cache directory on this platform")]
    CacheDirNotFound,

    #[error("could not prepare embedded Python source at {path}: {source}")]
    Extraction {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("could not invoke python3: {0}")]
    Invocation(#[source] io::Error),
}

/// Run the embedded Python CLI with the given argv tail.
///
/// Returns the exit code to propagate to the OS.
pub fn run(args: &[String]) -> Result<i32, ShimError> {
    let cache_base = cache_base()?;
    let source_root = ensure_extracted(&cache_base)?;
    invoke_python(&source_root, args)
}

fn cache_base() -> Result<PathBuf, ShimError> {
    Ok(dirs::cache_dir()
        .ok_or(ShimError::CacheDirNotFound)?
        .join("mergify")
        .join("py"))
}

/// Ensure the embedded Python source is present on disk under
/// `<cache_base>/<CACHE_KEY>/` and return that directory (which
/// contains a `mergify_cli/` subdirectory, ready for `PYTHONPATH`).
fn ensure_extracted(cache_base: &Path) -> Result<PathBuf, ShimError> {
    let target_dir = cache_base.join(CACHE_KEY);
    let sentinel = target_dir.join(".complete");

    // Fast path: already extracted.
    if sentinel.exists() {
        return Ok(target_dir);
    }

    fs::create_dir_all(cache_base).map_err(|source| ShimError::Extraction {
        path: cache_base.to_path_buf(),
        source,
    })?;

    // Lock a sibling file (not the target dir itself, which is about
    // to be renamed). Blocks until we have exclusive access so two
    // concurrent first-runs serialize on the extraction.
    let lock_path = cache_base.join(format!("{CACHE_KEY}.lock"));
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|source| ShimError::Extraction {
            path: lock_path.clone(),
            source,
        })?;
    FileExt::lock_exclusive(&lock_file).map_err(|source| ShimError::Extraction {
        path: lock_path.clone(),
        source,
    })?;

    // Double-check under lock: another process may have extracted
    // while we were waiting.
    if sentinel.exists() {
        return Ok(target_dir);
    }

    // Clean up any `*.extracting-*` dirs left behind by a crashed
    // previous attempt. We hold the lock, so no other process is
    // currently extracting for this cache key.
    let extracting_prefix = format!("{CACHE_KEY}.extracting-");
    if let Ok(entries) = fs::read_dir(cache_base) {
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(&extracting_prefix))
            {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }

    // Extract to a sibling temp dir, then atomic rename. Name
    // includes PID + a nanosecond timestamp so concurrent writers
    // (if any) don't collide even under an unlikely PID collision.
    let unique_suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let temp_dir = cache_base.join(format!(
        "{CACHE_KEY}.extracting-{}-{unique_suffix}",
        std::process::id(),
    ));
    fs::create_dir_all(&temp_dir).map_err(|source| ShimError::Extraction {
        path: temp_dir.clone(),
        source,
    })?;

    extract_into(&PY_SOURCE, &temp_dir.join("mergify_cli")).map_err(|source| {
        ShimError::Extraction {
            path: temp_dir.clone(),
            source,
        }
    })?;

    // Rename is atomic on the same filesystem. If a previous
    // interrupted run left a partial target, clear it first.
    if target_dir.exists() {
        fs::remove_dir_all(&target_dir).map_err(|source| ShimError::Extraction {
            path: target_dir.clone(),
            source,
        })?;
    }
    fs::rename(&temp_dir, &target_dir).map_err(|source| ShimError::Extraction {
        path: target_dir.clone(),
        source,
    })?;

    // Sentinel last — its presence means "extraction complete".
    fs::write(&sentinel, b"").map_err(|source| ShimError::Extraction {
        path: sentinel.clone(),
        source,
    })?;

    Ok(target_dir)
}

/// Write every file in `dir` under `target` (creating directories as
/// needed). `include_dir` stores each file's path relative to the
/// root Dir, so `target.join(file.path())` gives the full output
/// path even for deeply nested files.
fn extract_into(dir: &Dir<'_>, target: &Path) -> io::Result<()> {
    fs::create_dir_all(target)?;
    let mut stack: Vec<&Dir<'_>> = vec![dir];
    while let Some(current) = stack.pop() {
        for entry in current.entries() {
            match entry {
                DirEntry::Dir(subdir) => {
                    // subdir.path() is relative to the root Dir; strip
                    // the mergify_cli/ prefix the same way the file
                    // branch below does.
                    let relative = subdir
                        .path()
                        .strip_prefix(dir.path())
                        .unwrap_or(subdir.path());
                    fs::create_dir_all(target.join(relative))?;
                    stack.push(subdir);
                }
                DirEntry::File(file) => {
                    let relative = file.path().strip_prefix(dir.path()).unwrap_or(file.path());
                    let dest = target.join(relative);
                    if let Some(parent) = dest.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::write(dest, file.contents())?;
                }
            }
        }
    }
    Ok(())
}

fn invoke_python(source_root: &Path, args: &[String]) -> Result<i32, ShimError> {
    // Prepend the extracted dir to PYTHONPATH so `python3 -m
    // mergify_cli` resolves regardless of the user's environment.
    let mut paths = vec![source_root.to_path_buf()];
    if let Ok(existing) = env::var("PYTHONPATH") {
        paths.extend(env::split_paths(&existing));
    }
    let new_pythonpath = env::join_paths(paths).map_err(|e| {
        ShimError::Invocation(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("could not construct PYTHONPATH: {e}"),
        ))
    })?;

    let status = Command::new("python3")
        .arg("-m")
        .arg("mergify_cli")
        .args(args)
        .env("PYTHONPATH", new_pythonpath)
        // PYTHONSAFEPATH=1 stops Python from prepending the current
        // working directory to sys.path. Without it, a user running
        // from a directory that happens to contain a `mergify_cli/`
        // folder would import that instead of our extracted copy.
        // This repo's Python CLI requires Python 3.13+, which
        // supports PYTHONSAFEPATH (introduced in 3.11).
        .env("PYTHONSAFEPATH", "1")
        .status()
        .map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => ShimError::PythonNotFound,
            _ => ShimError::Invocation(e),
        })?;

    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_into_writes_embedded_files() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("mergify_cli");
        extract_into(&PY_SOURCE, &target).unwrap();

        // Spot-check a handful of files we know exist in the Python
        // source tree. Using files close to the root keeps the test
        // robust to refactors of subdirectories.
        assert!(target.join("__init__.py").is_file());
        assert!(target.join("cli.py").is_file());
        assert!(target.join("exit_codes.py").is_file());
    }

    #[test]
    fn extract_into_preserves_nested_structure() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("mergify_cli");
        extract_into(&PY_SOURCE, &target).unwrap();

        // Nested files get their full path reconstructed.
        assert!(target.join("ci").is_dir());
        assert!(target.join("stack").join("list.py").is_file());
    }

    #[test]
    fn ensure_extracted_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("cache");

        let first = ensure_extracted(&base).unwrap();
        let mtime_before = fs::metadata(first.join(".complete"))
            .unwrap()
            .modified()
            .unwrap();

        let second = ensure_extracted(&base).unwrap();
        let mtime_after = fs::metadata(second.join(".complete"))
            .unwrap()
            .modified()
            .unwrap();

        assert_eq!(first, second);
        // Sentinel is not rewritten on the fast path.
        assert_eq!(mtime_before, mtime_after);
    }
}
