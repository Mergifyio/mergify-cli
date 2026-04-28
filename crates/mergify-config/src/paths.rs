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

/// Resolve the path of the Mergify configuration file.
///
/// When ``explicit`` is ``Some``, that path must be a real file —
/// otherwise the user specified a bad path and we fail loudly with
/// [`CliError::Configuration`]. When ``explicit`` is ``None`` the
/// resolver walks [`DEFAULT_CONFIG_PATHS`] in order and returns the
/// first match.
pub fn resolve_config_path(explicit: Option<&Path>) -> Result<PathBuf, CliError> {
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
        let path = Path::new(candidate);
        if path.is_file() {
            return Ok(path.to_path_buf());
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
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let got = resolve_config_path(None).unwrap();
        assert_eq!(got, Path::new(".mergify.yml"));

        std::env::set_current_dir(prev).unwrap();
    }

    #[test]
    fn errors_when_no_file_and_no_explicit() {
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let err = resolve_config_path(None).unwrap_err();
        assert!(matches!(err, CliError::Configuration(_)));
        assert!(err.to_string().contains("not found"));

        std::env::set_current_dir(prev).unwrap();
    }

    #[test]
    fn errors_on_explicit_missing_file() {
        let err = resolve_config_path(Some(Path::new("/nonexistent/path.yml"))).unwrap_err();
        assert!(matches!(err, CliError::Configuration(_)));
    }
}
