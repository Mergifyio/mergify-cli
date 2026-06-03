//! `mergify ci scopes` — detect which scopes a build's changes
//! touch, based on file-pattern rules declared in `.mergify.yml`.
//!
//! The command is locally evaluated (no Mergify API call): it
//! loads the YAML config, figures out the `(base, head)` git
//! refs, diffs them for changed files, and walks each file
//! through every scope's include/exclude globs. The output is a
//! list of "touched" scopes plus a handful of CI-environment
//! side effects:
//!
//! - `$GITHUB_OUTPUT` — JSON dict `{scope: "true"|"false"}` under
//!   the `scopes` key, written as a multi-line heredoc.
//! - `$BUILDKITE` is `"true"` — same dict via `buildkite-agent
//!   meta-data set mergify-ci.scopes`.
//! - `$GITHUB_STEP_SUMMARY` — markdown table.
//! - `$BUILDKITE` is `"true"` — markdown via `buildkite-agent
//!   annotate` at both job and build scope.
//!
//! `--write <PATH>` writes a `{"scopes": [...]}` JSON file the
//! companion `ci scopes-send` command can consume — that's the
//! shape `DetectedScope` declares.

pub mod changed_files;
pub mod config;
pub mod matching;
pub mod outputs;

use std::env;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use mergify_core::CliError;
use mergify_core::Output;
use mergify_core::env::var_non_empty;
use serde::Serialize;

use crate::git_refs;
use crate::git_refs::References;
use crate::git_refs::ReferencesSource;

pub struct ScopesOptions<'a> {
    /// Explicit `--config <PATH>`. `None` triggers the
    /// fallback chain (env var `MERGIFY_CONFIG_PATH`, then auto-
    /// detection of `.mergify.yml` / `.mergify/config.yml` /
    /// `.github/mergify.yml`).
    pub config: Option<&'a Path>,
    /// Optional `--base`. Combined with `--head` to take the
    /// "manual" References branch.
    pub base: Option<&'a str>,
    /// Optional `--head`.
    pub head: Option<&'a str>,
    /// `--write/-w <PATH>` — write the detected scopes as JSON
    /// here. Skipped when `None`.
    pub write: Option<&'a Path>,
}

/// Wire-shape consumed by `ci scopes-send --scopes-json`.
/// Same JSON layout as Python's `DetectedScope` (a single
/// `scopes` array; the order is sorted because the in-memory
/// representation is a `BTreeSet`).
#[derive(Serialize)]
struct DetectedScope {
    scopes: Vec<String>,
}

/// Run the `ci scopes` command.
//
// `opts` is taken by value to match every other ported command's
// `run()` shape — clippy's `needless_pass_by_value` flags it
// because every field is a `Copy` reference, but flipping to
// `&ScopesOptions` here would make the dispatch table at the
// binary boundary asymmetric for no real win.
#[allow(clippy::needless_pass_by_value)]
pub fn run(opts: ScopesOptions<'_>, output: &mut dyn Output) -> Result<(), CliError> {
    let ScopesOptions {
        config,
        base,
        head,
        write,
    } = opts;
    let config_path = resolve_config_path(config)?;
    let cfg = config::load(&config_path)?;

    let refs = resolve_refs(base, head, output)?;
    emit_refs_header(&refs, output)?;

    let (all_scopes, mut scopes_hit, by_scope) = detect_scopes(&cfg.scopes, &refs, output)?;

    // Merge-queue scope is additive: it's part of `all_scopes`
    // unconditionally and gets added to `scopes_hit` only when the
    // refs came from MQ detection.
    let mut all_scopes = all_scopes;
    if !cfg.scopes.merge_queue_scope.is_empty() {
        all_scopes.insert(cfg.scopes.merge_queue_scope.clone());
        if refs.source == ReferencesSource::MergeQueue {
            scopes_hit.insert(cfg.scopes.merge_queue_scope.clone());
        }
    }

    emit_scopes_listing(&scopes_hit, &by_scope, output)?;

    outputs::maybe_write_github_outputs(&all_scopes, &scopes_hit)?;
    outputs::maybe_write_buildkite_metadata(&all_scopes, &scopes_hit)?;
    outputs::maybe_write_github_step_summary(&refs, &all_scopes, &scopes_hit)?;
    outputs::maybe_write_buildkite_annotation(&refs, &all_scopes, &scopes_hit);

    if let Some(write_path) = write {
        write_detected_scopes(write_path, &scopes_hit)?;
    }

    Ok(())
}

/// Auto-detection mirrors Python's
/// `detector.get_mergify_config_path` (which is the same triple
/// `.mergify.yml`, `.mergify/config.yml`, `.github/mergify.yml`
/// that `mergify config validate` uses), with `MERGIFY_CONFIG_PATH`
/// honored ahead of it. Empty env var falls back to auto-detect
/// — matches Python.
fn resolve_config_path(explicit: Option<&Path>) -> Result<PathBuf, CliError> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        return Err(CliError::Configuration(format!(
            "config file '{}' does not exist",
            path.display(),
        )));
    }
    if let Some(env_path) = var_non_empty("MERGIFY_CONFIG_PATH") {
        let p = PathBuf::from(&env_path);
        if !p.is_file() {
            return Err(CliError::Configuration(format!(
                "MERGIFY_CONFIG_PATH={env_path} does not point at a regular file",
            )));
        }
        return Ok(p);
    }
    mergify_config::paths::resolve_config_path(None)
}

/// `(base, head)` resolution mirrors Python's branch in
/// `scopes/cli.py`:
///
/// - At least one of `--base` / `--head` provided → "manual"
///   source, `head` defaults to `"HEAD"`.
/// - Neither provided → `git_refs::detect` with the production
///   notes reader (handles GHA / Buildkite / fallback).
fn resolve_refs(
    base: Option<&str>,
    head: Option<&str>,
    output: &mut dyn Output,
) -> Result<References, CliError> {
    if base.is_some() || head.is_some() {
        return Ok(References {
            base: base.map(ToString::to_string),
            head: head.unwrap_or("HEAD").to_string(),
            source: ReferencesSource::Manual,
        });
    }
    git_refs::detect(output, &git_refs::real_notes_reader)
}

/// Print the `Base: … / Head: … / Source: …` header lines via
/// the status sink (stderr in human mode, no-op in JSON mode).
/// Matches Python's `click.echo` of the same three lines.
fn emit_refs_header(refs: &References, output: &mut dyn Output) -> std::io::Result<()> {
    if let Some(base) = &refs.base {
        output.status(&format!("Base: {base}"))?;
    }
    output.status(&format!("Head: {head}", head = refs.head))?;
    output.status(&format!("Source: {source}", source = refs.source.as_str()))
}

type DetectResult = (
    std::collections::BTreeSet<String>,
    std::collections::BTreeSet<String>,
    std::collections::BTreeMap<String, Vec<String>>,
);

fn detect_scopes(
    scopes_cfg: &config::Scopes,
    refs: &References,
    output: &mut dyn Output,
) -> Result<DetectResult, CliError> {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;

    match &scopes_cfg.source {
        None => Ok((BTreeSet::new(), BTreeSet::new(), BTreeMap::new())),
        Some(config::Source::Manual(_)) => Err(CliError::Configuration(
            "source `manual` has been set, scopes must be sent with `scopes-send` or API"
                .to_string(),
        )),
        Some(config::Source::Files(files)) => {
            let all: BTreeSet<String> = files.files.keys().cloned().collect();

            // No base → "select all" branch, no git diff needed.
            // Matches Python's `if references.base is None`.
            let Some(base) = refs.base.as_deref() else {
                output.status("No base provided, selecting all scopes")?;
                return Ok((all.clone(), all, BTreeMap::new()));
            };

            let changed = changed_files::git_changed_files(base, &refs.head)?;
            output.status("Changed files detected:")?;
            for f in &changed {
                output.status(&format!("- {f}"))?;
            }
            let matchers = matching::compile(&files.files)?;
            let matching::MatchResult { hit, by_scope } =
                matching::route(changed.iter().map(String::as_str), &matchers);
            Ok((all, hit, by_scope))
        }
    }
}

/// Print "Scopes touched:" + sorted scope names, with the
/// per-file detail under `ACTIONS_STEP_DEBUG=true` (matches
/// Python's behavior so existing CI verbose-logs read the same).
fn emit_scopes_listing(
    hit: &std::collections::BTreeSet<String>,
    by_scope: &std::collections::BTreeMap<String, Vec<String>>,
    output: &mut dyn Output,
) -> Result<(), CliError> {
    let actions_debug = env::var("ACTIONS_STEP_DEBUG").as_deref() == Ok("true");
    if hit.is_empty() {
        output.status("No scopes matched.")?;
        return Ok(());
    }

    // Push the "Scopes touched:" block to stdout (so a downstream
    // pipe captures it) and the per-file dump to stderr — the
    // primary signal for downstream tooling is the list of
    // touched scopes.
    output.emit(&(), &mut |w: &mut dyn Write| {
        writeln!(w, "Scopes touched:")?;
        for s in hit {
            writeln!(w, "- {s}")?;
        }
        Ok(())
    })?;

    if actions_debug {
        for s in hit {
            if let Some(files) = by_scope.get(s) {
                for f in files {
                    output.status(&format!("    {f}"))?;
                }
            }
        }
    }
    Ok(())
}

fn write_detected_scopes(
    path: &Path,
    scopes: &std::collections::BTreeSet<String>,
) -> Result<(), CliError> {
    let payload = DetectedScope {
        scopes: scopes.iter().cloned().collect(),
    };
    let json = serde_json::to_string(&payload)
        .map_err(|e| CliError::Generic(format!("failed to serialize scopes JSON: {e}")))?;
    std::fs::write(path, json)
        .map_err(|e| CliError::Configuration(format!("cannot write {}: {e}", path.display())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::with_ci_env;
    use mergify_test_support::Captured;

    #[test]
    fn resolve_config_path_errors_on_missing_explicit() {
        let err = resolve_config_path(Some(Path::new("/no/such/file.yml"))).unwrap_err();
        assert!(matches!(err, CliError::Configuration(_)));
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn resolve_config_path_treats_empty_env_var_as_unset() {
        // Regression for the downstream `gha-mergify-ci` break
        // (monorepo#33423): the action sets `MERGIFY_CONFIG_PATH=""`
        // when no path was given, expecting auto-detect. Previously
        // clap's `env = "MERGIFY_CONFIG_PATH"` attribute on
        // `ci scopes --config` treated the empty env value as a
        // present-but-empty `--config` flag and aborted parsing
        // with "a value is required for '--config'", before this
        // function ever ran. The fix dropped the clap `env` hook
        // so this function owns the lookup — and the empty branch
        // here must fall through to autodetect rather than report
        // a malformed env var.
        let result = temp_env::with_var("MERGIFY_CONFIG_PATH", Some(""), || {
            resolve_config_path(None)
        });
        // Either autodetect found a real config (cargo test runs
        // from a workspace that contains `.mergify.yml`, so this is
        // the expected branch here) or it didn't — but the
        // env-var-specific error must not surface either way,
        // since "empty" means "not set" by contract.
        if let Err(err) = &result {
            let msg = err.to_string();
            assert!(
                !msg.contains("MERGIFY_CONFIG_PATH="),
                "empty env var leaked into the error message: {msg}",
            );
        }
    }

    #[test]
    fn resolve_config_path_errors_with_env_var_specific_message_when_set_but_invalid() {
        // Counterpart to the empty-env test: when the user (or a
        // wrapper script) sets `MERGIFY_CONFIG_PATH` to a real
        // value that doesn't exist, the error must name the env
        // var + the bogus path so the user can spot the typo
        // without having to dig.
        let err = temp_env::with_var("MERGIFY_CONFIG_PATH", Some("/no/such/.mergify.yml"), || {
            resolve_config_path(None).unwrap_err()
        });
        let msg = err.to_string();
        assert!(msg.contains("MERGIFY_CONFIG_PATH="), "got: {msg}");
        assert!(msg.contains("/no/such/.mergify.yml"), "got: {msg}");
    }

    #[test]
    fn write_detected_scopes_emits_sorted_json() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("scopes.json");
        let mut set = std::collections::BTreeSet::new();
        set.insert("zebra".to_string());
        set.insert("alpha".to_string());
        write_detected_scopes(&out, &set).unwrap();
        let raw = std::fs::read_to_string(&out).unwrap();
        // BTreeSet iteration is sorted; the JSON reflects it.
        assert_eq!(raw, r#"{"scopes":["alpha","zebra"]}"#);
    }

    #[test]
    fn run_selects_all_when_no_base_provided() {
        // Hermetic mirror of the live smoke test
        // `test_ci_scopes_select_all_when_no_base`: with
        // `--head HEAD` and no `--base`, the command takes the
        // "select all scopes" branch and reports every
        // configured scope as touched. No git operations.
        //
        // The `with_ci_env` wrapper scrubs `GITHUB_OUTPUT` so a
        // GHA runner executing the suite doesn't see `run()`
        // append a heredoc to its real step-output file (which
        // would break the runner step with "Matching delimiter
        // not found").
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("mergify.yml");
        std::fs::write(
            &cfg,
            "scopes:\n  source:\n    files:\n      backend:\n        include: ['src/**']\n      frontend:\n        include: ['web/**']\n",
        )
        .unwrap();
        let mut cap = Captured::human();
        with_ci_env(&[], || {
            run(
                ScopesOptions {
                    config: Some(&cfg),
                    base: None,
                    head: Some("HEAD"),
                    write: None,
                },
                &mut cap.output,
            )
            .unwrap();
        });
        let combined = cap.stdout() + &cap.stderr();
        for scope in ["backend", "frontend"] {
            assert!(combined.contains(scope), "missing scope: {combined}");
        }
        assert!(combined.contains("No base provided"));
    }

    #[test]
    fn run_errors_on_manual_source() {
        // `source: manual` is the "scopes-send / API only" mode;
        // running `ci scopes` against that config must abort
        // with a clear message (matches Python's
        // `ScopesError("source `manual` has been set ...")`).
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("mergify.yml");
        std::fs::write(&cfg, "scopes:\n  source:\n    manual: null\n").unwrap();
        let mut cap = Captured::human();
        let err = with_ci_env(&[], || {
            run(
                ScopesOptions {
                    config: Some(&cfg),
                    base: None,
                    head: Some("HEAD"),
                    write: None,
                },
                &mut cap.output,
            )
            .unwrap_err()
        });
        assert!(matches!(err, CliError::Configuration(_)));
        assert!(err.to_string().contains("scopes-send"), "got {err}");
    }

    #[test]
    fn run_writes_json_when_write_set() {
        // `--write <PATH>` writes a `{"scopes": [...]}` JSON
        // file the companion `ci scopes-send --scopes-json`
        // consumes. The no-base "select all" path is the easiest
        // way to populate scopes_hit without git operations.
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("mergify.yml");
        std::fs::write(
            &cfg,
            "scopes:\n  source:\n    files:\n      a:\n        include: ['*']\n      b:\n        include: ['*']\n",
        )
        .unwrap();
        let out = tmp.path().join("detected.json");
        let mut cap = Captured::human();
        with_ci_env(&[], || {
            run(
                ScopesOptions {
                    config: Some(&cfg),
                    base: None,
                    head: Some("HEAD"),
                    write: Some(&out),
                },
                &mut cap.output,
            )
            .unwrap();
        });
        let raw = std::fs::read_to_string(&out).unwrap();
        // BTreeSet ordering → alphabetical scopes in the file.
        // `merge-queue` is also added because the default
        // `merge_queue_scope` lands in `all_scopes` regardless.
        // But `scopes_hit` only includes it when the refs source
        // is MergeQueue, which isn't the case here.
        assert_eq!(raw, r#"{"scopes":["a","b"]}"#);
    }
}
