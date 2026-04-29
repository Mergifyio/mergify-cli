//! Shared helpers for resolving the Mergify configuration file path.
//!
//! Both `config validate` and `config simulate` accept a
//! ``--config-file`` flag and otherwise auto-detect the file from a
//! small list of conventional locations. The resolver here is the
//! single source of truth for that behavior.

use std::path::Path;
use std::path::PathBuf;

use mergify_core::CliError;

/// Filename patterns the CLI searches for a Mergify configuration,
/// in priority order. Mirrors ``MERGIFY_CONFIG_PATHS`` in
/// ``mergify_cli/ci/detector.py``.
pub const DEFAULT_CONFIG_PATHS: [&str; 3] =
    [".mergify.yml", ".mergify/config.yml", ".github/mergify.yml"];

/// Resolve the path of the Mergify configuration file relative to
/// the current working directory.
///
/// When ``explicit`` is ``Some``, that path must be a real file —
/// otherwise the user specified a bad path and we fail loudly with
/// [`CliError::Configuration`]. When ``explicit`` is ``None`` the
/// resolver walks [`DEFAULT_CONFIG_PATHS`] in order and returns the
/// first match.
pub fn resolve_config_path(explicit: Option<&Path>) -> Result<PathBuf, CliError> {
    resolve_config_path_in(explicit, Path::new("."))
}

/// Same as [`resolve_config_path`] but searches relative to
/// ``base`` instead of the current working directory.
///
/// Tests use this directly to avoid `std::env::set_current_dir`,
/// which races with parallel cargo test workers in the same
/// process.
///
/// # Errors
///
/// Returns [`CliError::Configuration`] when neither an explicit
/// path nor any default candidate exists.
pub fn resolve_config_path_in(explicit: Option<&Path>, base: &Path) -> Result<PathBuf, CliError> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        return Err(CliError::Configuration(format!(
            "Configuration file not found: {}",
            path.display(),
        )));
    }
    for candidate in DEFAULT_CONFIG_PATHS {
        let path = base.join(candidate);
        if path.is_file() {
            return Ok(path);
        }
    }
    Err(CliError::Configuration(format!(
        "Mergify configuration file not found. Looked in: {}",
        DEFAULT_CONFIG_PATHS.join(", "),
    )))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn finds_dotmergify_yml() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".mergify.yml"), "").unwrap();
        let got = resolve_config_path_in(None, tmp.path()).unwrap();
        assert_eq!(got, tmp.path().join(".mergify.yml"));
    }

    #[test]
    fn errors_when_no_file_and_no_explicit() {
        let tmp = tempfile::tempdir().unwrap();
        let err = resolve_config_path_in(None, tmp.path()).unwrap_err();
        assert!(matches!(err, CliError::Configuration(_)));
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn errors_on_explicit_missing_file() {
        let err = resolve_config_path_in(Some(Path::new("/nonexistent/path.yml")), Path::new("."))
            .unwrap_err();
        assert!(matches!(err, CliError::Configuration(_)));
    }
}
