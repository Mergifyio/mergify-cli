//! Side-effect emitters for `ci scopes`: GHA outputs, Buildkite
//! metadata, GitHub step summary, Buildkite annotation.
//!
//! Each stays quiet when its environment knob is absent.

use mergify_core::env;
use std::collections::BTreeSet;
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
    let payload = scopes_dict_json(all, hit);
    crate::github_output::append(&[(GITHUB_ACTIONS_OUTPUT_NAME, &payload)])
}

/// `buildkite-agent meta-data set mergify-ci.scopes <json>` when
/// `$BUILDKITE` is `"true"`. No-op otherwise.
pub fn maybe_write_buildkite_metadata(
    all: &BTreeSet<String>,
    hit: &BTreeSet<String>,
) -> Result<(), CliError> {
    if env::var("BUILDKITE").as_deref() != Some("true") {
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

/// Render a scope name as a Markdown table cell's code span.
///
/// A merge-queue batch's scopes can be names a CI reported, which
/// the engine only caps in length: a newline, `|` or backtick in one
/// would otherwise end the table or the span and let the name write
/// its own Markdown into the step summary and the Buildkite
/// annotation.
fn markdown_code(scope: &str) -> String {
    let escaped = scope.escape_debug().to_string().replace('|', "\\|");
    // A code span only closes on a backtick run of its own delimiter's
    // length, so the delimiter must be longer than any run inside.
    let longest_run = escaped.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    if longest_run == 0 {
        format!("`{escaped}`")
    } else {
        let fence = "`".repeat(longest_run + 1);
        format!("{fence} {escaped} {fence}")
    }
}

fn build_summary_markdown(
    refs: &References,
    all: &BTreeSet<String>,
    hit: &BTreeSet<String>,
) -> String {
    let mut md = String::from("## Mergify CI Scope Matching Results");
    if let Some(base) = refs.base.as_deref() {
        // Each ref is cut to 7 chars, git's abbreviated-SHA length.
        // Count chars, not bytes: a branch name can be non-ASCII, and
        // a byte slice would split a char. A ref name can also hold a
        // backtick or a newline, so it is escaped like a scope name.
        let base_short: String = base.chars().take(7).collect();
        let head_short: String = refs.head.chars().take(7).collect();
        let range = markdown_code(&format!("{base_short}...{head_short}"));
        let source = refs.source.as_str();
        let _ = write!(&mut md, " for {range} (source: `{source}`)");
    }
    md.push_str("\n\n| 🎯 Scope | ✅ Match |\n|:--|:--|\n");
    for scope in all {
        let emoji = if hit.contains(scope) { "✅" } else { "❌" };
        let _ = writeln!(&mut md, "| {} | {emoji} |", markdown_code(scope));
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
    let Some(path) = env::var_non_empty("GITHUB_STEP_SUMMARY") else {
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
    if env::var("BUILDKITE").as_deref() != Some("true") {
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
        if let Some(mut stdin) = child.stdin.take()
            && let Err(e) = stdin.write_all(md.as_bytes())
        {
            eprintln!(
                "warning: failed to pipe markdown to buildkite-agent annotate \
                     ({scope_label} scope): {e}",
            );
            // fall through to wait() so the child is reaped
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_refs::ReferencesSource;

    fn refs(base: Option<&str>, head: &str, source: ReferencesSource) -> References {
        References {
            base: base.map(ToString::to_string),
            head: head.to_string(),
            source,
            batch_scopes: None,
        }
    }

    #[test]
    fn summary_table_cannot_be_broken_out_of_by_a_reported_scope_name() {
        // Reported names are only length-capped by the engine.
        let evil = "x` |\n## All checks passed";
        let all: BTreeSet<String> = [evil.to_string(), "ok".to_string()].into();
        let hit = all.clone();
        let md = build_summary_markdown(
            &refs(None, "HEAD", ReferencesSource::MergeQueue),
            &all,
            &hit,
        );
        assert!(!md.lines().any(|l| l.starts_with("## All")), "{md}");
        assert!(
            md.contains(r"| `` x` \|\n## All checks passed `` | ✅ |"),
            "{md}"
        );
        assert!(md.contains("| `ok` | ✅ |"), "{md}");
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
        // `HEAD` and `HEAD^` are shorter than 7 chars. Regression
        // guard for `&base[..7]` on short refs.
        let r = refs(Some("HEAD^"), "HEAD", ReferencesSource::Manual);
        let all: BTreeSet<String> = ["a"].iter().map(|s| (*s).to_string()).collect();
        let hit = BTreeSet::new();
        let md = build_summary_markdown(&r, &all, &hit);
        assert!(
            md.contains("for `HEAD^...HEAD` (source: `manual`)"),
            "got:\n{md}"
        );
    }

    #[test]
    fn summary_markdown_truncates_non_ascii_ref_by_chars() {
        // Byte 7 of each ref falls inside a multi-byte char, so a
        // byte slice would panic.
        let r = refs(Some("releasé-2026"), "featuré/x", ReferencesSource::Manual);
        let all: BTreeSet<String> = ["a"].iter().map(|s| (*s).to_string()).collect();
        let hit = BTreeSet::new();
        let md = build_summary_markdown(&r, &all, &hit);
        assert!(md.contains("for `releasé...featuré`"), "got:\n{md}");
    }

    #[test]
    fn summary_markdown_cannot_be_broken_out_of_by_a_ref_name() {
        let r = refs(Some("a`\n## x"), "HEAD", ReferencesSource::Manual);
        let all: BTreeSet<String> = ["a"].iter().map(|s| (*s).to_string()).collect();
        let hit = BTreeSet::new();
        let md = build_summary_markdown(&r, &all, &hit);
        assert!(md.contains("for `` a`\\n## x...HEAD ``"), "got:\n{md}");
        assert!(!md.contains("\n## x"), "got:\n{md}");
    }

    #[test]
    fn markdown_code_delimiter_outlasts_consecutive_backticks() {
        // A two-backtick delimiter would be closed by the `` run.
        assert_eq!(markdown_code("a``<b>"), "``` a``<b> ```");
        assert_eq!(markdown_code("a`b``c"), "``` a`b``c ```");
    }
}
