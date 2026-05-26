//! YAML schema for the `scopes:` block in `.mergify.yml`.
//!
//! Mirrors `mergify_cli/ci/scopes/config/scopes.py`. Loaded once
//! at the top of [`super::run`] from whichever Mergify config the
//! user pointed at (explicit flag, env var, or auto-detected).

use mergify_core::CliError;
use serde::Deserialize;

/// Top-level Mergify config: we only care about the `scopes:`
/// block — every other section (queue rules, pull-request rules,
/// …) is forwarded to the engine and irrelevant here.
#[derive(Debug, Default, Deserialize)]
pub struct MergifyConfig {
    #[serde(default)]
    pub scopes: Scopes,
}

/// The `scopes:` block. Both fields have explicit defaults so a
/// minimal `scopes: {}` (or no `scopes:` at all) deserializes to
/// "no sources configured" + the default merge-queue scope name.
#[derive(Debug, Deserialize)]
pub struct Scopes {
    /// Where scopes come from. `None` disables source-based
    /// detection entirely (the command then only ever reports
    /// the merge-queue scope, if applicable).
    #[serde(default)]
    pub source: Option<Source>,

    /// Scope name automatically applied to merge-queue PRs.
    /// Defaults to `"merge-queue"`; matches Python. The explicit
    /// `Default` impl below keeps the constant in one place —
    /// `#[derive(Default)]` would silently produce `String::new()`
    /// when the whole `scopes:` block is missing from the YAML.
    #[serde(default = "default_merge_queue_scope")]
    pub merge_queue_scope: String,
}

impl Default for Scopes {
    fn default() -> Self {
        Self {
            source: None,
            merge_queue_scope: default_merge_queue_scope(),
        }
    }
}

fn default_merge_queue_scope() -> String {
    "merge-queue".to_string()
}

/// `source:` is a one-of: either a file-pattern map or the
/// `manual: null` sentinel that means "scopes are sent via
/// `scopes-send` or the API directly".
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Source {
    Files(SourceFiles),
    Manual(SourceManual),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceFiles {
    /// Scope-name → file filters. `BTreeMap` gives us sorted-by-key
    /// iteration for free — Python sorts scope names before
    /// printing anyway, so this both matches behavior and keeps the
    /// human output deterministic without pulling in `indexmap`.
    pub files: std::collections::BTreeMap<String, FileFilters>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManual {
    /// Sentinel field — the YAML literal is `manual: null`. Stored
    /// as `Option<()>` so the field is optional and explicit-null
    /// both parse.
    #[allow(dead_code)]
    pub manual: Option<()>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileFilters {
    /// Glob patterns of files to include. Empty means "include
    /// everything before exclusions" (matches Python's default
    /// of `("**/*",)` — we keep the same default here).
    #[serde(default = "default_include")]
    pub include: Vec<String>,

    /// Glob patterns of files to exclude. Applied after `include`
    /// and takes precedence.
    #[serde(default)]
    pub exclude: Vec<String>,
}

fn default_include() -> Vec<String> {
    vec!["**/*".to_string()]
}

/// Parse a Mergify config file from `path`, surfacing parse
/// errors as [`CliError::Configuration`] so the binary exits with
/// the right code (8) and an obvious "your YAML is broken"
/// message.
pub fn load(path: &std::path::Path) -> Result<MergifyConfig, CliError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| CliError::Configuration(format!("cannot read {}: {e}", path.display())))?;
    serde_yaml_ng::from_str(&text)
        .map_err(|e| CliError::Configuration(format!("invalid YAML in {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> MergifyConfig {
        serde_yaml_ng::from_str(yaml).expect("yaml parses")
    }

    #[test]
    fn empty_yaml_yields_defaults() {
        // No `scopes:` block at all — the deserializer must still
        // produce a usable `MergifyConfig` with `source: None` and
        // the default merge-queue scope name.
        let cfg: MergifyConfig = parse("{}");
        assert!(cfg.scopes.source.is_none());
        assert_eq!(cfg.scopes.merge_queue_scope, "merge-queue");
    }

    #[test]
    fn source_files_round_trip() {
        let cfg = parse(
            r"
scopes:
  source:
    files:
      backend:
        include: ['mergify_cli/**']
        exclude: ['mergify_cli/tests/**']
      frontend:
        include: ['web/**']
",
        );
        let Some(Source::Files(files)) = cfg.scopes.source else {
            panic!("expected files source");
        };
        assert_eq!(files.files.len(), 2);
        assert_eq!(
            files.files["backend"].include,
            vec!["mergify_cli/**".to_string()],
        );
        assert_eq!(
            files.files["backend"].exclude,
            vec!["mergify_cli/tests/**".to_string()],
        );
        assert_eq!(files.files["frontend"].include, vec!["web/**".to_string()]);
        assert!(files.files["frontend"].exclude.is_empty());
    }

    #[test]
    fn source_files_defaults_include_to_everything() {
        // Mirror Python's `default_factory=lambda: ("**/*",)` —
        // omitting `include` means "include everything before
        // exclusions".
        let cfg = parse(
            r"
scopes:
  source:
    files:
      everything:
        exclude: ['*.md']
",
        );
        let Some(Source::Files(files)) = cfg.scopes.source else {
            panic!("expected files source");
        };
        assert_eq!(files.files["everything"].include, vec!["**/*".to_string()]);
    }

    #[test]
    fn source_manual_round_trip() {
        let cfg = parse(
            r"
scopes:
  source:
    manual: null
",
        );
        assert!(matches!(cfg.scopes.source, Some(Source::Manual(_))));
    }

    #[test]
    fn merge_queue_scope_override() {
        let cfg = parse(
            r"
scopes:
  merge_queue_scope: mq-bypass
",
        );
        assert_eq!(cfg.scopes.merge_queue_scope, "mq-bypass");
    }

    #[test]
    fn unknown_field_in_files_block_rejected() {
        // `deny_unknown_fields` on `SourceFiles` catches typos
        // like `file:` instead of `files:` — the parse must fail,
        // not silently fall back to an empty map.
        let err = serde_yaml_ng::from_str::<MergifyConfig>(
            r"
scopes:
  source:
    file:
      backend: {}
",
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("did not match any variant") || msg.contains("unknown field"),
            "unexpected error message: {msg}"
        );
    }
}
