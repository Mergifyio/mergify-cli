//! `mergify ci scopes` — detect which scopes a build's changes
//! touch, based on file-pattern rules declared in `.mergify.yml`.
//!
//! The command is locally evaluated (no Mergify API call): it
//! loads the YAML config, figures out the `(base, head)` git
//! refs, diffs them for changed files, and walks each file
//! through every scope's include/exclude globs. On a merge-queue
//! draft whose engine git note carries the batch's scopes, it
//! skips the diff and returns those instead: the queue already
//! decided them from each batched pull request, including scopes
//! a pull request's CI reported rather than matched from files.
//! The output is a list of "touched" scopes plus a handful of
//! CI-environment side effects:
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

use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use mergify_core::CliError;
use mergify_core::Output;
use mergify_core::env;
use serde::Serialize;

use crate::git_refs;
use crate::git_refs::References;
use crate::git_refs::ReferencesSource;

pub struct ScopesOptions<'a> {
    /// Explicit `--config <PATH>`. `None` triggers the
    /// fallback chain (env var `MERGIFY_CONFIG_PATH`, then the
    /// auto-detection over
    /// [`mergify_config::paths::DEFAULT_CONFIG_PATHS`]).
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
    let config_path = resolve_config_path(config, output)?;
    let cfg = config::load(&config_path)?;

    let refs = resolve_refs(base, head, output)?;
    run_on_refs(&cfg, &refs, write, output)
}

/// Everything `run` does once the config is loaded and the refs
/// resolved; split out so tests can hand it merge-queue refs
/// without a git note to read them from.
fn run_on_refs(
    cfg: &config::MergifyConfig,
    refs: &References,
    write: Option<&Path>,
    output: &mut dyn Output,
) -> Result<(), CliError> {
    emit_refs_header(refs, output)?;

    let (all_scopes, mut scopes_hit, by_scope) = match &refs.batch_scopes {
        Some(batch) => {
            output.status("Scopes decided by the merge queue for this batch")?;
            let all: std::collections::BTreeSet<String> = declared_scopes(&cfg.scopes)
                .union(&batch.scopes)
                .cloned()
                .collect();
            // A barrier impacts every scope, so every scope is tested,
            // not just the concrete ones its pulls named.
            let hit = if batch.all_scopes {
                output.status("The batch is a merge queue barrier, selecting all scopes")?;
                all.clone()
            } else {
                batch.scopes.clone()
            };
            (all, hit, std::collections::BTreeMap::new())
        }
        None => detect_scopes(&cfg.scopes, refs, output)?,
    };

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
    outputs::maybe_write_github_step_summary(refs, &all_scopes, &scopes_hit)?;
    outputs::maybe_write_buildkite_annotation(refs, &all_scopes, &scopes_hit);

    if let Some(write_path) = write {
        write_detected_scopes(write_path, &scopes_hit)?;
    }

    Ok(())
}

/// Auto-detection defers to [`mergify_config::paths`], so
/// `ci scopes` searches exactly what `mergify config validate`
/// searches — including the duplicate-configuration warning.
/// `MERGIFY_CONFIG_PATH` is honored ahead of it; an empty value
/// falls back to auto-detect, which is what the `gha-mergify-ci`
/// action relies on.
fn resolve_config_path(
    explicit: Option<&Path>,
    output: &mut dyn Output,
) -> Result<PathBuf, CliError> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        return Err(CliError::Configuration(format!(
            "config file '{}' does not exist",
            path.display(),
        )));
    }
    if let Some(env_path) = env::var_non_empty("MERGIFY_CONFIG_PATH") {
        let p = PathBuf::from(&env_path);
        if !p.is_file() {
            return Err(CliError::Configuration(format!(
                "MERGIFY_CONFIG_PATH={env_path} does not point at a regular file",
            )));
        }
        return Ok(p);
    }
    mergify_config::paths::resolve_config_path(None, output)
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
            batch_scopes: None,
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

/// The scopes `.mergify.yml` declares, which the outputs list
/// even when not hit. Only a `files` source declares any: `manual`
/// scopes exist once some CI reports them.
fn declared_scopes(scopes_cfg: &config::Scopes) -> std::collections::BTreeSet<String> {
    match &scopes_cfg.source {
        Some(config::Source::Files(files)) => files.files.keys().cloned().collect(),
        Some(config::Source::Manual(_)) | None => std::collections::BTreeSet::new(),
    }
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
            let all = declared_scopes(scopes_cfg);

            // No base → "select all" branch, no git diff needed.
            // Matches Python's `if references.base is None`.
            let Some(base) = refs.base.as_deref() else {
                output.status("No base provided, selecting all scopes")?;
                return Ok((all.clone(), all, BTreeMap::new()));
            };

            let changed = changed_files::git_changed_files(None, base, &refs.head)?;
            output.status("Changed files detected:")?;
            for f in &changed {
                output.status(&format!("- {}", display_untrusted(f)))?;
            }
            let matchers = matching::compile(&files.files)?;
            let matching::MatchResult { hit, by_scope } =
                matching::route(changed.iter().map(String::as_str), &matchers);
            Ok((all, hit, by_scope))
        }
    }
}

/// Render a repo path, or a scope name a CI reported, for a log line.
///
/// `git_changed_files` returns paths as git holds them, and a
/// filename may legally contain a newline or an ANSI escape. Echoed
/// raw into a GitHub Actions log, a name like
/// `evil\n::error::pwned.txt` starts a line at column 0 that the
/// runner parses as a workflow command and turns into a fabricated
/// annotation — `::add-mask::` and `::stop-commands::` are reachable
/// the same way. `escape_debug` keeps printable Unicode readable
/// (`café.txt` stays `café.txt`) while neutralizing the control
/// characters, which is exactly what git's own `core.quotePath`
/// output did for us before this module started passing `-z`.
fn display_untrusted(path: &str) -> String {
    path.escape_debug().to_string()
}

/// Print "Scopes touched:" + sorted scope names, with the
/// per-file detail under `ACTIONS_STEP_DEBUG=true` (matches
/// Python's behavior so existing CI verbose-logs read the same).
fn emit_scopes_listing(
    hit: &std::collections::BTreeSet<String>,
    by_scope: &std::collections::BTreeMap<String, Vec<String>>,
    output: &mut dyn Output,
) -> Result<(), CliError> {
    let actions_debug = env::var("ACTIONS_STEP_DEBUG").as_deref() == Some("true");
    if hit.is_empty() {
        output.status("No scopes matched.")?;
        return Ok(());
    }

    // Push the "Scopes touched:" block to stdout (so a downstream
    // pipe captures it). Under ACTIONS_STEP_DEBUG each scope's
    // matching files are listed (sorted) immediately under their
    // scope name, interleaved on the same stream — matching
    // Python's `click.echo` ordering so verbose CI logs read the
    // same.
    output.emit(&(), &mut |w: &mut dyn Write| {
        writeln!(w, "Scopes touched:")?;
        for s in hit {
            // A merge-queue batch's scopes can be names a CI reported,
            // which the engine does not constrain to the config's
            // scope-name pattern: same hazard as a path.
            writeln!(w, "- {}", display_untrusted(s))?;
            if actions_debug && let Some(files) = by_scope.get(s) {
                let mut files: Vec<&String> = files.iter().collect();
                files.sort();
                for f in files {
                    writeln!(w, "    {}", display_untrusted(f))?;
                }
            }
        }
        Ok(())
    })?;
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
    // A failed write is a runtime I/O failure (exit 1), not a
    // configuration problem (exit 8): the config parsed fine, the
    // output path is just unwritable. Keep the OS error as the cause.
    std::fs::write(path, json)
        .map_err(|e| CliError::wrap(format!("cannot write {}", path.display()), e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mergify_test_support::Captured;

    #[test]
    fn display_untrusted_neutralizes_workflow_command_injection() {
        // Since `git_changed_files` passes `-z`, git no longer
        // escapes control characters for us, and a filename may
        // legally hold a newline. Echoed raw, the runner would read
        // the second line as a workflow command and annotate a
        // passing job with a fabricated error.
        let evil = "critical/evil\n::error::pwned.txt";
        let rendered = display_untrusted(evil);
        assert!(!rendered.contains('\n'), "got: {rendered}");
        assert!(rendered.contains("\\n::error::"), "got: {rendered}");
        // An ANSI escape can't repaint the operator's terminal.
        assert_eq!(display_untrusted("a\u{1b}[31mb"), "a\\u{1b}[31mb");
        // Ordinary printable Unicode stays readable — the listing
        // is for humans reading a CI log.
        assert_eq!(
            display_untrusted("critical/caché.txt"),
            "critical/caché.txt"
        );
    }

    #[test]
    fn resolve_config_path_errors_on_missing_explicit() {
        let mut captured = Captured::human();
        let err = resolve_config_path(Some(Path::new("/no/such/file.yml")), &mut captured.output)
            .unwrap_err();
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
        let mut captured = Captured::human();
        let result = env::testing::with_var("MERGIFY_CONFIG_PATH", Some(""), || {
            resolve_config_path(None, &mut captured.output)
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
        let mut captured = Captured::human();
        let err =
            env::testing::with_var("MERGIFY_CONFIG_PATH", Some("/no/such/.mergify.yml"), || {
                resolve_config_path(None, &mut captured.output).unwrap_err()
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
    fn write_detected_scopes_io_failure_is_a_runtime_error() {
        // Writing under a non-existent directory fails with a plain
        // I/O error. That's a runtime failure (exit 1), not a config
        // problem (exit 8) — the config itself was fine.
        let tmp = tempfile::tempdir().unwrap();
        let unwritable = tmp
            .path()
            .join("no")
            .join("such")
            .join("dir")
            .join("scopes.json");
        let mut set = std::collections::BTreeSet::new();
        set.insert("backend".to_string());
        let err = write_detected_scopes(&unwritable, &set).unwrap_err();
        assert_eq!(err.exit_code(), mergify_core::ExitCode::GenericError);
        assert!(err.to_string().contains("cannot write"), "got: {err}");
    }

    #[test]
    fn run_selects_all_when_no_base_provided() {
        // Hermetic mirror of the live smoke test
        // `test_ci_scopes_select_all_when_no_base`: with
        // `--head HEAD` and no `--base`, the command takes the
        // "select all scopes" branch and reports every
        // configured scope as touched. No git operations.
        //
        // The empty overlay hides `GITHUB_OUTPUT` so a GHA runner
        // executing the suite doesn't see `run()` append a heredoc
        // to its real step-output file (which would break the
        // runner step with "Matching delimiter not found").
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("mergify.yml");
        std::fs::write(
            &cfg,
            "scopes:\n  source:\n    files:\n      backend:\n        include: ['src/**']\n      frontend:\n        include: ['web/**']\n",
        )
        .unwrap();
        let mut cap = Captured::human();
        env::testing::with_no_vars(|| {
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
        let err = env::testing::with_no_vars(|| {
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
        env::testing::with_no_vars(|| {
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

    fn merge_queue_refs(batch: &[&str]) -> References {
        References {
            base: Some("cafef00dcafef00dcafef00dcafef00dcafef00d".into()),
            head: "HEAD".into(),
            source: ReferencesSource::MergeQueue,
            batch_scopes: Some(git_refs::BatchScopes {
                scopes: batch.iter().map(|s| (*s).to_string()).collect(),
                all_scopes: false,
            }),
        }
    }

    #[test]
    fn merge_queue_barrier_selects_every_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg: config::MergifyConfig = serde_yaml_ng::from_str(
            "scopes:\n  source:\n    files:\n      a:\n        include: ['*']\n      c:\n        include: ['*']\n",
        )
        .unwrap();
        let mut refs = merge_queue_refs(&["reported"]);
        refs.batch_scopes.as_mut().unwrap().all_scopes = true;
        let write = tmp.path().join("detected.json");
        let mut cap = Captured::human();
        env::testing::with_no_vars(|| {
            run_on_refs(&cfg, &refs, Some(&write), &mut cap.output).unwrap();
        });
        assert_eq!(
            std::fs::read_to_string(&write).unwrap(),
            r#"{"scopes":["a","c","merge-queue","reported"]}"#,
        );
    }

    #[test]
    fn merge_queue_batch_scopes_replace_the_diff_in_every_output() {
        // The refs point at a base that does not exist, so a diff
        // would fail the run: reaching the outputs proves none ran.
        // `c` is declared but not in the batch, `reported` is in the
        // batch but not declared (a scope some CI reported).
        let tmp = tempfile::tempdir().unwrap();
        let cfg: config::MergifyConfig = serde_yaml_ng::from_str(
            "scopes:\n  source:\n    files:\n      a:\n        include: ['*']\n      c:\n        include: ['*']\n",
        )
        .unwrap();
        let gha = tmp.path().join("gha_output");
        let write = tmp.path().join("detected.json");
        let mut cap = Captured::human();
        env::testing::with_vars([("GITHUB_OUTPUT", Some(gha.to_str().unwrap()))], || {
            run_on_refs(
                &cfg,
                &merge_queue_refs(&["a", "reported"]),
                Some(&write),
                &mut cap.output,
            )
            .unwrap();
        });
        assert_eq!(
            std::fs::read_to_string(&write).unwrap(),
            r#"{"scopes":["a","merge-queue","reported"]}"#,
        );
        let gha = std::fs::read_to_string(&gha).unwrap();
        assert!(
            gha.contains(
                r#"{"a": "true", "c": "false", "merge-queue": "true", "reported": "true"}"#
            ),
            "got: {gha}",
        );
        assert!(!cap.stderr().contains("Changed files"), "{}", cap.stderr());
    }

    #[test]
    fn merge_queue_batch_scopes_serve_a_manual_source() {
        // `manual` has nothing to diff against, so off the merge
        // queue it refuses; on a batch the queue already knows.
        let tmp = tempfile::tempdir().unwrap();
        let cfg: config::MergifyConfig =
            serde_yaml_ng::from_str("scopes:\n  source:\n    manual: null\n").unwrap();
        let write = tmp.path().join("detected.json");
        let mut cap = Captured::human();
        env::testing::with_no_vars(|| {
            run_on_refs(&cfg, &merge_queue_refs(&[]), Some(&write), &mut cap.output).unwrap();
        });
        assert_eq!(
            std::fs::read_to_string(&write).unwrap(),
            r#"{"scopes":["merge-queue"]}"#,
        );
    }

    #[test]
    fn batch_scope_names_cannot_inject_a_workflow_command() {
        let cfg = config::MergifyConfig::default();
        let mut cap = Captured::human();
        env::testing::with_no_vars(|| {
            run_on_refs(
                &cfg,
                &merge_queue_refs(&["x\n::error::pwned"]),
                None,
                &mut cap.output,
            )
            .unwrap();
        });
        let stdout = cap.stdout();
        assert!(!stdout.lines().any(|l| l.starts_with("::")), "{stdout:?}");
        assert!(stdout.contains(r"x\n::error::pwned"), "{stdout:?}");
    }
}
