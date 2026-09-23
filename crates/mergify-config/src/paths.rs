//! Shared helpers for resolving the Mergify configuration file path.
//!
//! Both `config validate` and `config simulate` accept a
//! ``--config-file`` flag and otherwise auto-detect the file from a
//! small list of conventional locations. The resolver here is the
//! single source of truth for that behavior — `mergify ci scopes`
//! falls back to it too, so there is one search order in the CLI.

use std::path::Path;
use std::path::PathBuf;

use mergify_core::CliError;
use mergify_core::Output;

/// Filename patterns the CLI searches for a Mergify configuration,
/// in priority order: the three conventional locations, and at each
/// of them the `.yml` spelling ahead of the `.yaml` one. The first
/// file that exists wins.
///
/// The engine must search the same list in the same order
/// (MRGFY-9532). A repository carrying several of these files is
/// otherwise validated against one file and merged against another,
/// which is the one failure `config validate` exists to prevent.
pub const DEFAULT_CONFIG_PATHS: [&str; 6] = [
    ".mergify.yml",
    ".mergify.yaml",
    ".mergify/config.yml",
    ".mergify/config.yaml",
    ".github/mergify.yml",
    ".github/mergify.yaml",
];

/// The candidate that won the search, plus the ones it beat.
struct Resolution {
    path: PathBuf,
    /// Candidates that exist on disk but lose to `path` under the
    /// [`DEFAULT_CONFIG_PATHS`] order. Always empty for an explicit
    /// `--config-file`: the user named the file, nothing is ignored.
    shadowed: Vec<PathBuf>,
}

impl Resolution {
    /// The warning to hand the user when the repository carries more
    /// than one configuration file, or `None` when it carries one.
    ///
    /// Naming the losers matters more than naming the winner: an
    /// edit to an ignored file looks like Mergify dropping a change.
    fn shadow_warning(&self) -> Option<String> {
        if self.shadowed.is_empty() {
            return None;
        }
        let ignored = self
            .shadowed
            .iter()
            .map(|p| format!("'{}'", p.display()))
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!(
            "mergify: warning: several Mergify configuration files found; \
             using '{}' and ignoring {ignored}.",
            self.path.display(),
        ))
    }
}

/// Resolve the path of the Mergify configuration file relative to
/// the current working directory.
///
/// When ``explicit`` is ``Some``, that path must be a real file —
/// otherwise the user specified a bad path and we fail loudly with
/// [`CliError::Configuration`]. When ``explicit`` is ``None`` the
/// resolver walks [`DEFAULT_CONFIG_PATHS`] in order and returns the
/// first match, warning through ``output`` about any other candidate
/// it found on the way.
///
/// # Errors
///
/// Returns [`CliError::Configuration`] when neither an explicit
/// path nor any default candidate exists.
pub fn resolve_config_path(
    explicit: Option<&Path>,
    output: &mut dyn Output,
) -> Result<PathBuf, CliError> {
    resolve_config_path_in(explicit, Path::new("."), output)
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
pub fn resolve_config_path_in(
    explicit: Option<&Path>,
    base: &Path,
    output: &mut dyn Output,
) -> Result<PathBuf, CliError> {
    let resolution = resolve_in(explicit, base)?;
    // `Output::status` is the house channel for human chatter, and
    // it writes to stderr so `--json` stdout stays a single
    // document. It is a no-op in `OutputMode::Json`, which none of
    // the config-reading commands run in today.
    if let Some(warning) = resolution.shadow_warning() {
        output.status(&warning)?;
    }
    Ok(resolution.path)
}

fn resolve_in(explicit: Option<&Path>, base: &Path) -> Result<Resolution, CliError> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(Resolution {
                path: path.to_path_buf(),
                shadowed: Vec::new(),
            });
        }
        return Err(CliError::Configuration(format!(
            "Configuration file not found: {}",
            path.display(),
        )));
    }
    let mut found = DEFAULT_CONFIG_PATHS
        .iter()
        .map(|candidate| base.join(candidate))
        .filter(|path| path.is_file());
    let Some(path) = found.next() else {
        return Err(CliError::Configuration(format!(
            "Mergify configuration file not found. Looked in: {}",
            DEFAULT_CONFIG_PATHS.join(", "),
        )));
    };
    Ok(Resolution {
        path,
        shadowed: found.collect(),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use mergify_test_support::Captured;

    use super::*;

    /// Resolve under `base` with a capturing output, returning the
    /// winner and whatever was written to stderr.
    fn resolve(base: &Path) -> (Result<PathBuf, CliError>, String) {
        let mut captured = Captured::human();
        let got = resolve_config_path_in(None, base, &mut captured.output);
        (got, captured.stderr())
    }

    #[test]
    fn finds_dotmergify_yml() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".mergify.yml"), "").unwrap();
        let (got, stderr) = resolve(tmp.path());
        assert_eq!(got.unwrap(), tmp.path().join(".mergify.yml"));
        assert_eq!(stderr, "");
    }

    #[test]
    fn finds_dotmergify_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".mergify.yaml"), "").unwrap();
        let (got, stderr) = resolve(tmp.path());
        assert_eq!(got.unwrap(), tmp.path().join(".mergify.yaml"));
        assert_eq!(stderr, "");
    }

    #[test]
    fn finds_yaml_in_every_conventional_location() {
        for candidate in [
            ".mergify.yaml",
            ".mergify/config.yaml",
            ".github/mergify.yaml",
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join(candidate);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "").unwrap();
            let (got, _) = resolve(tmp.path());
            assert_eq!(got.unwrap(), path, "looking for {candidate}");
        }
    }

    #[test]
    fn yml_wins_over_yaml_at_the_same_location() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".mergify.yml"), "").unwrap();
        fs::write(tmp.path().join(".mergify.yaml"), "").unwrap();
        let (got, stderr) = resolve(tmp.path());
        assert_eq!(got.unwrap(), tmp.path().join(".mergify.yml"));
        assert_eq!(
            stderr,
            format!(
                "mergify: warning: several Mergify configuration files found; \
                 using '{}' and ignoring '{}'.\n",
                tmp.path().join(".mergify.yml").display(),
                tmp.path().join(".mergify.yaml").display(),
            ),
        );
    }

    /// The location order outranks the extension order: a `.yaml`
    /// higher up beats a `.yml` lower down.
    #[test]
    fn location_order_outranks_extension_order() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".mergify.yaml"), "").unwrap();
        fs::create_dir_all(tmp.path().join(".github")).unwrap();
        fs::write(tmp.path().join(".github/mergify.yml"), "").unwrap();
        let (got, _) = resolve(tmp.path());
        assert_eq!(got.unwrap(), tmp.path().join(".mergify.yaml"));
    }

    /// Every loser is named, in search order, on one line.
    #[test]
    fn warning_names_every_ignored_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".mergify.yml"), "").unwrap();
        fs::write(tmp.path().join(".mergify.yaml"), "").unwrap();
        fs::create_dir_all(tmp.path().join(".github")).unwrap();
        fs::write(tmp.path().join(".github/mergify.yaml"), "").unwrap();
        let (_, stderr) = resolve(tmp.path());
        assert_eq!(
            stderr,
            format!(
                "mergify: warning: several Mergify configuration files found; \
                 using '{}' and ignoring '{}', '{}'.\n",
                tmp.path().join(".mergify.yml").display(),
                tmp.path().join(".mergify.yaml").display(),
                tmp.path().join(".github/mergify.yaml").display(),
            ),
        );
    }

    #[test]
    fn explicit_path_is_never_reported_as_shadowing() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".mergify.yml"), "").unwrap();
        let explicit = tmp.path().join(".mergify.yaml");
        fs::write(&explicit, "").unwrap();
        let mut captured = Captured::human();
        let got = resolve_config_path_in(Some(&explicit), tmp.path(), &mut captured.output);
        assert_eq!(got.unwrap(), explicit);
        assert_eq!(captured.stderr(), "");
    }

    #[test]
    fn errors_when_no_file_and_no_explicit() {
        let tmp = tempfile::tempdir().unwrap();
        let (got, _) = resolve(tmp.path());
        let err = got.unwrap_err();
        assert!(matches!(err, CliError::Configuration(_)));
        assert!(err.to_string().contains("not found"));
        // The failure lists what was searched, so a user who spelled
        // the file `.yaml` can see that spelling is accepted.
        assert!(err.to_string().contains(".mergify.yaml"));
    }

    #[test]
    fn errors_on_explicit_missing_file() {
        let mut captured = Captured::human();
        let err = resolve_config_path_in(
            Some(Path::new("/nonexistent/path.yml")),
            Path::new("."),
            &mut captured.output,
        )
        .unwrap_err();
        assert!(matches!(err, CliError::Configuration(_)));
    }
}
