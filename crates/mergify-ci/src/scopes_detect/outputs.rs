//! Side-effect emitters for `ci scopes`: GHA outputs, Buildkite
//! metadata, GitHub step summary, Buildkite annotation.
//!
//! All four mirror their Python counterparts in
//! `mergify_cli/ci/scopes/cli.py` and stay quiet when their
//! respective environment knob is absent.

use std::collections::BTreeSet;
use std::env;
use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;

use mergify_core::CliError;

use crate::git_refs::References;

const GITHUB_ACTIONS_OUTPUT_NAME: &str = "scopes";
const BUILDKITE_SCOPES_METADATA_KEY: &str = "mergify-ci.scopes";
const BUILDKITE_ANNOTATION_CONTEXT: &str = "mergify-ci-scopes";

/// Build the GHA-style scopes payload: `{scope: "true"|"false"}`
/// with stable string-valued booleans. Mirrors Python's
/// "GHA outputs are strings; copying a bool through workflows
/// converts to the literal string `false|true`, so we make it a
/// string up front to avoid the user-visible mismatch."
fn scopes_dict_json(all: &BTreeSet<String>, hit: &BTreeSet<String>) -> String {
    // Build by hand so the key order is the sorted ordering from
    // the BTreeSet — `serde_json::Map` randomizes hash maps and
    // a serialized object's key order would drift between runs.
    let mut out = String::from("{");
    for (i, scope) in all.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        // `serde_json::to_string` on a `&str` quotes + escapes per
        // JSON rules — safer than manual `\"{scope}\"` for scope
        // names with control characters (the schema disallows
        // them, but defense in depth costs nothing).
        let key = serde_json::to_string(scope).expect("scope name serializes");
        let value = if hit.contains(scope) { "true" } else { "false" };
        let _ = write!(&mut out, "{key}: \"{value}\"");
    }
    out.push('}');
    out
}

/// Append `scopes<<delimiter\n{json}\ndelimiter\n` to
/// `$GITHUB_OUTPUT` when the env var is set. No-op otherwise.
pub fn maybe_write_github_outputs(
    all: &BTreeSet<String>,
    hit: &BTreeSet<String>,
) -> Result<(), CliError> {
    let Some(path) = env::var("GITHUB_OUTPUT").ok().filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    let delimiter = format!("ghadelimiter_{}", random_delimiter_suffix()?);
    let payload = scopes_dict_json(all, hit);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(PathBuf::from(path))?;
    writeln!(file, "{GITHUB_ACTIONS_OUTPUT_NAME}<<{delimiter}")?;
    writeln!(file, "{payload}")?;
    writeln!(file, "{delimiter}")?;
    Ok(())
}

/// `buildkite-agent meta-data set mergify-ci.scopes <json>` when
/// `$BUILDKITE` is `"true"`. No-op otherwise.
pub fn maybe_write_buildkite_metadata(
    all: &BTreeSet<String>,
    hit: &BTreeSet<String>,
) -> Result<(), CliError> {
    if env::var("BUILDKITE").as_deref() != Ok("true") {
        return Ok(());
    }
    let payload = scopes_dict_json(all, hit);
    let status = Command::new("buildkite-agent")
        .args(["meta-data", "set", BUILDKITE_SCOPES_METADATA_KEY, &payload])
        .status()
        .map_err(|e| CliError::Generic(format!("failed to spawn `buildkite-agent`: {e}")))?;
    if !status.success() {
        return Err(CliError::Generic(format!(
            "`buildkite-agent meta-data set` exited with status {status}",
        )));
    }
    Ok(())
}

fn build_summary_markdown(
    refs: &References,
    all: &BTreeSet<String>,
    hit: &BTreeSet<String>,
) -> String {
    let mut md = String::from("## Mergify CI Scope Matching Results");
    if let Some(base) = refs.base.as_deref() {
        // Python truncates each ref to 7 chars (git's standard
        // abbreviated SHA). When the ref is shorter than 7 chars
        // (e.g. `HEAD`) we'd panic; clamp the slice length.
        let base_short = &base[..base.len().min(7)];
        let head_short = &refs.head[..refs.head.len().min(7)];
        let source = refs.source.as_str();
        let _ = write!(
            &mut md,
            " for `{base_short}...{head_short}` (source: `{source}`)",
        );
    }
    md.push_str("\n\n| 🎯 Scope | ✅ Match |\n|:--|:--|\n");
    for scope in all {
        let emoji = if hit.contains(scope) { "✅" } else { "❌" };
        let _ = writeln!(&mut md, "| `{scope}` | {emoji} |");
    }
    md
}

/// Append the scope-matching markdown table to
/// `$GITHUB_STEP_SUMMARY` when the env var is set. No-op
/// otherwise.
pub fn maybe_write_github_step_summary(
    refs: &References,
    all: &BTreeSet<String>,
    hit: &BTreeSet<String>,
) -> Result<(), CliError> {
    let Some(path) = env::var("GITHUB_STEP_SUMMARY")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        return Ok(());
    };
    let md = build_summary_markdown(refs, all, hit);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(PathBuf::from(path))?;
    file.write_all(md.as_bytes())?;
    Ok(())
}

/// `buildkite-agent annotate` with the same markdown, at both job
/// and build scope. Failures are non-fatal — warns on stderr and
/// continues, matching Python. The repeated context guarantees
/// re-runs update in place rather than duplicate.
pub fn maybe_write_buildkite_annotation(
    refs: &References,
    all: &BTreeSet<String>,
    hit: &BTreeSet<String>,
) {
    if env::var("BUILDKITE").as_deref() != Ok("true") {
        return;
    }
    let md = build_summary_markdown(refs, all, hit);
    for (scope_label, extra_arg) in &[("job", Some("--scope=job")), ("build", None)] {
        let mut cmd = Command::new("buildkite-agent");
        cmd.args([
            "annotate",
            "--style",
            "info",
            "--context",
            BUILDKITE_ANNOTATION_CONTEXT,
        ]);
        if let Some(arg) = extra_arg {
            cmd.arg(arg);
        }
        let result = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match result {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "warning: failed to spawn buildkite-agent annotate ({scope_label} scope): {e}",
                );
                continue;
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = stdin.write_all(md.as_bytes()) {
                eprintln!(
                    "warning: failed to pipe markdown to buildkite-agent annotate \
                     ({scope_label} scope): {e}",
                );
                // fall through to wait() so the child is reaped
            }
        }
        let output = match child.wait_with_output() {
            Ok(o) => o,
            Err(e) => {
                eprintln!(
                    "warning: failed to wait for buildkite-agent annotate \
                     ({scope_label} scope): {e}",
                );
                continue;
            }
        };
        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr);
            eprintln!(
                "warning: failed to write Buildkite annotation ({scope_label} scope): {}",
                detail.trim(),
            );
        }
    }
}

/// 32 random hex chars from the OS RNG. Same approach as
/// `ci queue-info` for the heredoc delimiter — `uuid` would do
/// the job but carries more crate surface than we need for a
/// throwaway delimiter.
fn random_delimiter_suffix() -> Result<String, CliError> {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf)
        .map_err(|e| CliError::Generic(format!("OS random source unavailable: {e}")))?;
    let mut hex = String::with_capacity(buf.len() * 2);
    for b in buf {
        let _ = write!(hex, "{b:02x}");
    }
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_refs::ReferencesSource;

    fn refs(base: Option<&str>, head: &str, source: ReferencesSource) -> References {
        References {
            base: base.map(ToString::to_string),
            head: head.to_string(),
            source,
        }
    }

    #[test]
    fn scopes_dict_json_keys_sorted_alphabetically() {
        let all: BTreeSet<String> = ["zebra", "alpha", "mid"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let hit: BTreeSet<String> = ["mid"].iter().map(|s| (*s).to_string()).collect();
        let out = scopes_dict_json(&all, &hit);
        // BTreeSet sorts; the JSON must reflect that.
        assert_eq!(
            out,
            r#"{"alpha": "false", "mid": "true", "zebra": "false"}"#,
        );
    }

    #[test]
    fn summary_markdown_omits_range_when_no_base() {
        // `ci scopes --head HEAD` (no `--base`) takes the
        // "select all" branch; the markdown should still render
        // but skip the `for `……` (source: …)` suffix because
        // there's no range to point at.
        let r = refs(None, "HEAD", ReferencesSource::Manual);
        let all: BTreeSet<String> = ["a", "b"].iter().map(|s| (*s).to_string()).collect();
        let hit: BTreeSet<String> = ["a"].iter().map(|s| (*s).to_string()).collect();
        let md = build_summary_markdown(&r, &all, &hit);
        assert!(
            md.starts_with("## Mergify CI Scope Matching Results\n\n"),
            "got:\n{md}",
        );
        assert!(md.contains("| `a` | ✅ |"), "got:\n{md}");
        assert!(md.contains("| `b` | ❌ |"), "got:\n{md}");
    }

    #[test]
    fn summary_markdown_includes_range_when_base_present() {
        let r = refs(
            Some("0123456789abcdef0123456789abcdef01234567"),
            "fedcba9876543210fedcba9876543210fedcba98",
            ReferencesSource::GithubEventPullRequest,
        );
        let all: BTreeSet<String> = ["a"].iter().map(|s| (*s).to_string()).collect();
        let hit: BTreeSet<String> = BTreeSet::new();
        let md = build_summary_markdown(&r, &all, &hit);
        assert!(
            md.contains("for `0123456...fedcba9` (source: `github_event_pull_request`)"),
            "got:\n{md}",
        );
    }

    #[test]
    fn summary_markdown_short_ref_does_not_panic() {
        // `HEAD` and `HEAD^` are shorter than 7 chars; the slice
        // must clamp instead of panicking. Regression guard for
        // `&base[..7]` on short refs.
        let r = refs(Some("HEAD^"), "HEAD", ReferencesSource::Manual);
        let all: BTreeSet<String> = ["a"].iter().map(|s| (*s).to_string()).collect();
        let hit = BTreeSet::new();
        let _md = build_summary_markdown(&r, &all, &hit);
    }
}
