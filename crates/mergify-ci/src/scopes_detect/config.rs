//! YAML schema for the `scopes:` block in `.mergify.yml`.
//!
//! The structs below are the typed data model the detection logic
//! reads from. They are deliberately *not* the validation authority:
//! the user's `scopes:` block is first checked against the engine's
//! generated JSON schema (`schemas/mergify-config-schema.json`, kept
//! in sync by the ci-bot `schemas-sync.yml` workflow), so the CLI
//! rejects exactly what the engine rejects — including the scope-name
//! constraints (`^[A-Za-z0-9_-]+$`, min length 2) that serde alone
//! cannot express. Loaded once at the top of [`super::run`] from
//! whichever Mergify config the user pointed at (explicit flag, env
//! var, or auto-detected).

use mergify_core::CliError;
use serde::Deserialize;

/// The engine-generated Mergify config JSON schema, vendored verbatim
/// from `monorepo/engine/schemas/mergify-config-schema.json` and kept
/// in sync by the ci-bot `schemas-sync.yml` workflow. Embedded at
/// build time so validation needs no filesystem or network access in
/// the CI hot path. We only validate the `scopes:` block against its
/// `#/$defs/Scopes` subschema — every other config section is the
/// engine's concern, not the CLI's.
const CONFIG_SCHEMA: &str = include_str!("../../schemas/mergify-config-schema.json");

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
///
/// `deny_unknown_fields` mirrors the engine model's `extra="forbid"`
/// and the sibling structs below: a typo'd key under `scopes:` is a
/// hard error, not a silently-dropped field. Schema validation in
/// [`load`] catches this too, but keeping the struct strict means the
/// standalone deserialize path stays honest on its own.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
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

/// Parse a Mergify config file from `path`, surfacing parse and
/// schema-validation errors as [`CliError::Configuration`] so the
/// binary exits with the right code (8) and an obvious "your config
/// is broken" message.
///
/// Validation runs in two passes:
/// 1. The `scopes:` block is checked against the engine-generated
///    JSON schema (better, engine-aligned errors — e.g. a bad scope
///    name names the offending value).
/// 2. The whole file is deserialized into [`MergifyConfig`] for the
///    typed model the detection logic reads from.
///
/// Schema validation runs first so the user sees the precise schema
/// error rather than serde's opaque "did not match any variant".
pub fn load(path: &std::path::Path) -> Result<MergifyConfig, CliError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| CliError::Configuration(format!("cannot read {}: {e}", path.display())))?;
    validate_scopes(&text, path)?;
    serde_yaml_ng::from_str(&text)
        .map_err(|e| CliError::Configuration(format!("invalid YAML in {}: {e}", path.display())))
}

/// Validate the `scopes:` block of `text` against the engine's
/// `#/$defs/Scopes` subschema.
///
/// A missing `scopes:` key is valid (defaults apply). YAML that does
/// not even parse to a value is left for the [`load`] deserialize
/// step to report, so we don't surface the same syntax error twice.
fn validate_scopes(text: &str, path: &std::path::Path) -> Result<(), CliError> {
    // If the YAML doesn't parse as a value at all, let the typed
    // deserialize in `load` produce the canonical syntax error.
    let Ok(doc) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(text) else {
        return Ok(());
    };
    let Some(scopes) = doc.get("scopes") else {
        return Ok(());
    };
    // jsonschema validates JSON, so convert the YAML node. The
    // conversion is lossless for the scalars/maps/sequences a scopes
    // block can contain.
    let scopes_json: serde_json::Value = serde_json::to_value(scopes).map_err(|e| {
        CliError::Configuration(format!(
            "cannot convert scopes block of {} for validation: {e}",
            path.display(),
        ))
    })?;

    let validator = scopes_validator();
    let mut errors: Vec<String> = validator
        .iter_errors(&scopes_json)
        .map(|err| {
            let loc = err.instance_path().to_string();
            let loc = loc.trim_start_matches('/').replace('/', ".");
            if loc.is_empty() {
                format!("scopes: {err}")
            } else {
                format!("scopes.{loc}: {err}")
            }
        })
        .collect();
    // `iter_errors` order is unspecified; sort so the CLI prints the
    // same lines in the same order on every run (matches the
    // deterministic output of `mergify config validate`).
    errors.sort();

    if errors.is_empty() {
        Ok(())
    } else {
        Err(CliError::Configuration(format!(
            "invalid scopes config in {}:\n  - {}",
            path.display(),
            errors.join("\n  - "),
        )))
    }
}

/// Build a validator for the `Scopes` definition out of the vendored
/// config schema. We wrap a `$ref` to `#/$defs/Scopes` around the
/// schema's full `$defs` so the nested `SourceFiles`/`SourceManual`/
/// `FileFilters` references resolve within the document.
fn scopes_validator() -> jsonschema::Validator {
    let full: serde_json::Value =
        serde_json::from_str(CONFIG_SCHEMA).expect("vendored config schema is valid JSON");
    let defs = full
        .get("$defs")
        .cloned()
        .expect("vendored config schema has a $defs section");
    let subschema = serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$ref": "#/$defs/Scopes",
        "$defs": defs,
    });
    jsonschema::options()
        .build(&subschema)
        .expect("scopes subschema compiles")
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

    fn validate(yaml: &str) -> Result<(), CliError> {
        validate_scopes(yaml, std::path::Path::new("test.yml"))
    }

    #[test]
    fn schema_accepts_valid_scopes() {
        validate(
            r"
scopes:
  source:
    files:
      backend:
        include: ['src/**']
        exclude: ['*.md']
  merge_queue_scope: mq
",
        )
        .expect("valid scopes pass the engine schema");
    }

    #[test]
    fn schema_accepts_missing_scopes_block() {
        // No `scopes:` key — defaults apply, nothing to validate.
        validate("pull_request_rules: []").expect("no scopes block is valid");
    }

    #[test]
    fn schema_defers_unparseable_yaml_to_deserialize() {
        // Broken YAML must not be reported here; `load`'s typed
        // deserialize owns the canonical syntax error so the user
        // doesn't see it twice.
        validate("not: valid: yaml: [").expect("unparseable YAML is deferred, not double-reported");
    }

    #[test]
    fn schema_rejects_short_scope_name() {
        // `propertyNames.minLength = 2` — a one-char scope name is
        // rejected by the engine schema even though serde accepts any
        // map key.
        let err = validate(
            r"
scopes:
  source:
    files:
      a:
        include: ['src/**']
",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("invalid scopes config"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn schema_rejects_unknown_filter_field() {
        // `FileFilters` is `additionalProperties: false` — a typo'd
        // key (`includes` for `include`) is caught by the schema, the
        // same gate `deny_unknown_fields` gives the typed structs.
        let err = validate(
            r"
scopes:
  source:
    files:
      backend:
        includes: ['src/**']
",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("invalid scopes config"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn schema_rejects_bad_merge_queue_scope() {
        // `merge_queue_scope` carries the same name constraints.
        let err = validate(
            r"
scopes:
  merge_queue_scope: 'has space'
",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("invalid scopes config"),
            "unexpected error: {err}"
        );
    }
}
