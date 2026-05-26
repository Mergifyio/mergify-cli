//! `mergify ci junit-process` orchestration.
//!
//! Glues the four `junit_process` modules together: parse `JUnit`
//! XML → check quarantine status → build OTLP spans (now tagged
//! with `cicd.test.quarantined`) → upload them, then render the
//! human-facing report the way Python's `process_junit_files`
//! does. Errors during quarantine or upload are *non-fatal* by
//! design — the report calls them out but the overall exit code
//! is driven by the failing-tests-not-quarantined count plus the
//! silent-failure detection.

// The report builder appends formatted snippets to a single
// `String`. clippy's `format_push_string` lint suggests `write!`
// everywhere, which adds a `use std::fmt::Write` and an awkward
// `let _ = write!(…)` per line for no semantic improvement —
// `String::push_str(&format!(…))` is the readable form for this
// kind of templated text emission.
#![allow(clippy::format_push_string)]

use std::env;
use std::path::{Path, PathBuf};

use mergify_core::{CliError, ExitCode, Output};
use url::Url;

use crate::detector;
use crate::junit_process::junit::{self, ParseResult, TestCase};
use crate::junit_process::quarantine::{self, QuarantineFailed, QuarantineResult};
use crate::junit_process::spans::{self, UploadMetadata};
use crate::junit_process::upload;

const SEPARATOR: &str = "══════════════════════════════════════════";
const SEPARATOR_LIGHT: &str = "──────────────────────────────────────────";

/// CLI options for `mergify ci junit-process`. Mirrors the
/// Python flag set — see `mergify_cli/ci/cli.py`.
pub struct JunitProcessOptions<'a> {
    pub api_url: Option<&'a str>,
    pub token: Option<&'a str>,
    pub repository: Option<&'a str>,
    pub test_framework: Option<&'a str>,
    pub test_language: Option<&'a str>,
    pub tests_target_branch: Option<&'a str>,
    pub test_exit_code: Option<i32>,
    /// Raw `files` arguments as the user typed them. Globs (`**`,
    /// `*`, `?`) are expanded here, matching Python's
    /// `_expand_junit_patterns` callback.
    pub files: &'a [String],
}

/// Run the command. Returns an [`ExitCode`] reflecting the final
/// verdict so the caller can plumb it through to the process
/// exit. Network failures (quarantine / upload) do NOT propagate
/// as errors — they print to the report and the run continues.
/// The only `Err` paths are argument resolution failures (e.g.
/// missing token) and unrecoverable input errors (no XML, parse
/// failure on every file).
#[allow(clippy::too_many_lines)] // Straight-line orchestration: parse →
// quarantine → build spans → upload → render. Splitting this into
// helpers spreads the report builder's ordering across the file
// without buying anything you can't already see by scrolling.
pub async fn run(
    opts: JunitProcessOptions<'_>,
    output: &mut dyn Output,
) -> Result<ExitCode, CliError> {
    // ── Resolve required inputs up front so we fail before
    // printing any of the banner — matches Python's click defaults
    // surfacing as exit code 2 when a required flag is missing,
    // before the command body runs.
    let api_url_raw = resolve_api_url(opts.api_url);
    let api_url = Url::parse(&api_url_raw)
        .map_err(|e| CliError::Configuration(format!("--api-url is not a valid URL: {e}")))?;
    let token = resolve_token(opts.token)?;
    let repository = resolve_repository(opts.repository)?;
    let tests_target_branch = resolve_tests_target_branch(opts.tests_target_branch)?;
    let files = expand_files(opts.files)?;

    // ── Header (printed regardless of outcome).
    let mut report = String::new();
    write_header(&mut report);

    // ── Parse. A parse failure aborts before the upload step
    // (Python returns ExitCode.GENERIC_ERROR with an inline error
    // banner; we do the same).
    let parsed = match parse_all(&files) {
        Ok(p) => p,
        Err(err) => {
            write_early_exit(
                &mut report,
                &format!("Failed to parse JUnit XML: {err}"),
                "Check that your test framework is generating valid JUnit XML output.",
            );
            emit(output, &report)?;
            return Ok(ExitCode::GenericError);
        }
    };

    if parsed.cases.is_empty() {
        write_early_exit(
            &mut report,
            "No spans found in the JUnit files",
            "Check that the JUnit XML files are not empty.",
        );
        emit(output, &report)?;
        return Ok(ExitCode::GenericError);
    }

    // ── Quarantine check (best effort). Failures here don't
    // abort — we fall back to "treat every failure as blocking".
    let quarantine_result = quarantine::check_failing(
        &api_url,
        &token,
        &repository,
        &tests_target_branch,
        &parsed.cases,
    )
    .await;

    let (quarantine_result, quarantine_error) = match quarantine_result {
        Ok(r) => (r, None::<String>),
        Err(QuarantineFailed { message }) => (
            // Conservative fallback: every failure is treated as
            // blocking, none as quarantined.
            blocking_fallback(&parsed.cases),
            Some(message),
        ),
    };

    // ── Build spans + upload (best effort). The quarantined set
    // gets folded into each case span's `cicd.test.quarantined`
    // attribute at build time, so we don't need to re-tag after
    // the fact.
    let metadata = UploadMetadata {
        test_framework: opts.test_framework.map(str::to_string),
        test_language: opts.test_language.map(str::to_string),
        mergify_test_job_name: env::var("MERGIFY_TEST_JOB_NAME")
            .ok()
            .filter(|s| !s.is_empty()),
        quarantined: quarantine_result
            .quarantined
            .iter()
            .map(|c| c.name.clone())
            .collect(),
    };
    let built = spans::build_traces(&parsed, &metadata);

    let client = upload::default_client();
    let upload_error =
        match upload::upload(&client, &api_url_raw, &token, &repository, &built.request).await {
            Ok(()) => None,
            Err(err) => Some(err.to_string()),
        };

    // ── Report sections — order matches Python verbatim.
    write_run_id(&mut report, &built.run_id);

    let total_cases = count_test_cases(&parsed);
    let nb_failures = count_failures(&parsed);
    write_upload_summary(
        &mut report,
        files.len(),
        total_cases,
        nb_failures,
        upload_error.is_some(),
    );

    if let Some(err) = &upload_error {
        write_upload_error_block(&mut report, err);
    }

    write_quarantine_section(&mut report, &quarantine_result, quarantine_error.as_deref());

    // ── Silent-failure detection. If the test runner exited
    // non-zero but the JUnit report has no failures, the runner
    // probably crashed — fail loudly so the user knows the report
    // is incomplete.
    if let Some(exit_code) = opts.test_exit_code {
        if exit_code != 0 && nb_failures == 0 {
            write_silent_failure(&mut report, exit_code);
            emit(output, &report)?;
            return Ok(ExitCode::GenericError);
        }
    }

    // ── Verdict.
    let final_failure_message =
        quarantine_failure_message(&quarantine_result, nb_failures, quarantine_error.is_some());
    let nb_quarantined_failures = quarantine_result.failing.len();
    write_verdict(
        &mut report,
        final_failure_message.as_deref(),
        nb_quarantined_failures,
    );

    emit(output, &report)?;

    Ok(if final_failure_message.is_some() {
        ExitCode::GenericError
    } else {
        ExitCode::Success
    })
}

fn emit(output: &mut dyn Output, report: &str) -> Result<(), CliError> {
    output
        .emit(&(), &mut |w| w.write_all(report.as_bytes()))
        .map_err(|e| CliError::Generic(format!("could not write output: {e}")))
}

fn resolve_api_url(explicit: Option<&str>) -> String {
    if let Some(v) = explicit.filter(|s| !s.is_empty()) {
        return v.to_string();
    }
    if let Ok(v) = env::var("MERGIFY_API_URL") {
        if !v.is_empty() {
            return v;
        }
    }
    "https://api.mergify.com".to_string()
}

fn resolve_token(explicit: Option<&str>) -> Result<String, CliError> {
    if let Some(v) = explicit.filter(|s| !s.is_empty()) {
        return Ok(v.to_string());
    }
    env::var("MERGIFY_TOKEN")
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            CliError::Configuration(
                "--token not provided and MERGIFY_TOKEN env var is empty".to_string(),
            )
        })
}

fn resolve_repository(explicit: Option<&str>) -> Result<String, CliError> {
    if let Some(v) = explicit.filter(|s| !s.is_empty()) {
        return Ok(v.to_string());
    }
    detector::get_github_repository().ok_or_else(|| {
        CliError::Configuration(
            "--repository not provided and could not be detected from the CI environment"
                .to_string(),
        )
    })
}

fn resolve_tests_target_branch(explicit: Option<&str>) -> Result<String, CliError> {
    let raw = if let Some(v) = explicit.filter(|s| !s.is_empty()) {
        v.to_string()
    } else {
        detector::get_tests_target_branch().ok_or_else(|| {
            CliError::Configuration(
                "--tests-target-branch not provided and could not be detected".to_string(),
            )
        })?
    };
    // Mirror Python's `_process_tests_target_branch` callback that
    // strips `refs/heads/` so the branch name matches what the
    // quarantine API expects.
    Ok(raw.strip_prefix("refs/heads/").unwrap_or(&raw).to_string())
}

/// Expand each `files` entry: existing literal paths take
/// precedence over glob expansion (so `report[1].xml` keeps
/// working), then `**`/`*`/`?` patterns get expanded. A glob that
/// matches nothing is a hard error — same as Python's behavior,
/// since "no test reports" almost always means a previous CI step
/// silently failed and we want the user to see it.
fn expand_files(raw: &[String]) -> Result<Vec<PathBuf>, CliError> {
    if raw.is_empty() {
        return Err(CliError::Configuration(
            "at least one JUnit XML file path is required".to_string(),
        ));
    }
    // Preserve insertion order while deduplicating — `Vec<PathBuf>`
    // is small (a handful of XML reports per CI step), so a linear
    // `contains` check on each insert is cheaper than a `BTreeSet`
    // and keeps ordering deterministic across runs.
    let mut out: Vec<PathBuf> = Vec::new();
    for entry in raw {
        let literal = Path::new(entry);
        if literal.is_file() {
            if !out.iter().any(|p| p == literal) {
                out.push(literal.to_path_buf());
            }
            continue;
        }
        if has_glob_magic(entry) {
            let matches = glob::glob(entry).map_err(|e| {
                CliError::Configuration(format!("invalid glob pattern {entry:?}: {e}"))
            })?;
            let mut any_match = false;
            for m in matches {
                let path = m.map_err(|e| {
                    CliError::Configuration(format!("glob walk failed for {entry:?}: {e}"))
                })?;
                if !path.is_file() {
                    continue;
                }
                if !out.iter().any(|p| p == &path) {
                    out.push(path);
                }
                any_match = true;
            }
            if !any_match {
                return Err(CliError::Configuration(format!(
                    "Pattern '{entry}' did not match any file.\n\n\
                     This usually indicates that a previous CI step failed to generate the test results.\n\
                     Please check if your test execution step completed successfully and produced the expected output files."
                )));
            }
            continue;
        }
        if literal.is_dir() {
            return Err(CliError::Configuration(format!(
                "'{entry}' is a directory, not a JUnit XML file.\n\n\
                 Pass a file path or a quoted glob pattern (e.g. 'reports/**/*.xml') instead."
            )));
        }
        return Err(CliError::Configuration(format!(
            "JUnit XML file '{entry}' does not exist.\n\n\
             This usually indicates that a previous CI step failed to generate the test results.\n\
             Please check if your test execution step completed successfully and produced the expected output file."
        )));
    }
    Ok(out)
}

fn has_glob_magic(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

fn parse_all(files: &[PathBuf]) -> Result<ParseResult, junit::InvalidJunitXml> {
    // Concatenate the cases from every file. The OTLP layer
    // doesn't care which file a case came from — JUnit suites
    // already group them, and that grouping is what becomes a
    // suite span downstream.
    let mut all_cases = Vec::new();
    for path in files {
        let bytes = std::fs::read(path).map_err(|e| junit::InvalidJunitXml {
            details: format!("cannot read {}: {e}", path.display()),
        })?;
        let parsed = junit::parse(&bytes)?;
        all_cases.extend(parsed.cases);
    }
    Ok(ParseResult { cases: all_cases })
}

fn count_test_cases(parsed: &ParseResult) -> usize {
    parsed.cases.len()
}

fn count_failures(parsed: &ParseResult) -> usize {
    parsed
        .cases
        .iter()
        .filter(|c| c.status.is_failure())
        .count()
}

fn blocking_fallback(cases: &[TestCase]) -> QuarantineResult {
    let failing: Vec<TestCase> = cases
        .iter()
        .filter(|c| c.status.is_failure())
        .cloned()
        .collect();
    let count = failing.len();
    QuarantineResult {
        non_quarantined: failing.clone(),
        failing,
        quarantined: Vec::new(),
        failing_not_quarantined_count: count,
    }
}

fn quarantine_failure_message(
    result: &QuarantineResult,
    nb_failures: usize,
    quarantine_errored: bool,
) -> Option<String> {
    if quarantine_errored {
        return Some(format!(
            "Treating {nb_failures}/{nb_failures} failures as blocking"
        ));
    }
    if result.failing_not_quarantined_count > 0 {
        let count = result.failing_not_quarantined_count;
        let total = result.failing.len();
        let quarantined = total - count;
        return Some(format!("{quarantined}/{total} failures quarantined"));
    }
    None
}

fn write_header(out: &mut String) {
    out.push_str(SEPARATOR);
    out.push('\n');
    out.push_str("  🚀 CI Insights\n");
    out.push('\n');
    out.push_str(concat!(
        "  Uploads JUnit test results to Mergify CI Insights and evaluates\n",
        "  quarantine status for failing tests. This step determines the\n",
        "  final CI status — quarantined failures are ignored.\n",
        "  Learn more: https://docs.mergify.com/ci-insights/quarantine\n",
    ));
    out.push_str(SEPARATOR);
    out.push('\n');
}

fn write_run_id(out: &mut String, run_id: &str) {
    out.push('\n');
    out.push_str(&format!("  Run ID: {run_id}\n"));
}

fn write_upload_summary(
    out: &mut String,
    reports: usize,
    tests: usize,
    failures: usize,
    upload_failed: bool,
) {
    let reports_label = format!(
        "{reports} {kind}",
        kind = if reports == 1 { "report" } else { "reports" }
    );
    let failures_label = format!(
        "{failures} {kind}",
        kind = if failures == 1 { "failure" } else { "failures" }
    );
    if upload_failed {
        out.push_str(&format!("      ☁️ {reports_label} not uploaded\n"));
    } else {
        out.push_str(&format!("      ☁️ {reports_label} uploaded\n"));
    }
    out.push_str(&format!("      🧪 {tests} tests ({failures_label})\n"));
}

fn write_upload_error_block(out: &mut String, error: &str) {
    out.push_str("\n  ⚠️ Failed to upload test results\n");
    out.push_str("    Mergify CI Insights won't process these test results.\n");
    out.push_str("    Quarantine status and CI outcome are unaffected.\n");
    out.push('\n');
    out.push_str("      ┌ Details\n");
    for line in error.lines() {
        out.push_str(&format!("      │  {line}\n"));
    }
    out.push_str("      └─\n");
}

fn write_quarantine_section(out: &mut String, result: &QuarantineResult, error: Option<&str>) {
    // Skip the whole section when there's nothing to say — Python
    // does the same `if not result.failing_spans and error is None`
    // early return.
    if result.failing.is_empty() && error.is_none() {
        return;
    }
    out.push('\n');
    out.push_str(SEPARATOR_LIGHT);
    out.push('\n');
    out.push('\n');
    out.push_str("🛡️ Quarantine\n");

    if let Some(err) = error {
        out.push('\n');
        out.push_str("  ⚠️ Failed to check quarantine status\n");
        out.push_str("    Contact Mergify support if this error persists.\n");
        out.push('\n');
        out.push_str("      ┌ Details\n");
        for line in err.lines() {
            out.push_str(&format!("      │  {line}\n"));
        }
        out.push_str("      └─\n");
    }

    if !result.quarantined.is_empty() {
        out.push_str(&format!(
            "\n  🔒 Quarantined ({n}):\n",
            n = result.quarantined.len()
        ));
        for case in &result.quarantined {
            out.push_str(&format!("      · {name}\n", name = case.name));
        }
    }

    if !result.non_quarantined.is_empty() {
        let label = if error.is_some() {
            "Could not verify quarantine status"
        } else {
            "Unquarantined"
        };
        out.push_str(&format!(
            "\n  ❌ {label} ({n}):\n",
            n = result.non_quarantined.len()
        ));
        for case in &result.non_quarantined {
            write_failure_block(out, case);
        }
    }
}

fn write_failure_block(out: &mut String, case: &TestCase) {
    out.push_str(&format!("\n      ┌ {name}\n", name = case.name));
    let f = &case.failure;
    if f.kind.is_none() && f.message.is_none() && f.stacktrace.is_none() {
        out.push_str("      │\n");
        out.push_str("      │  (no error details in JUnit report)\n");
        out.push_str("      └─\n");
        return;
    }
    if f.kind.is_some() || f.message.is_some() {
        let parts: Vec<&str> = [f.kind.as_deref(), f.message.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        out.push_str("      │\n");
        out.push_str(&format!("      │  {joined}\n", joined = parts.join(": ")));
    }
    if let Some(stack) = &f.stacktrace {
        out.push_str("      │\n");
        for line in stack.lines() {
            out.push_str(&format!("      │  {line}\n"));
        }
    }
    out.push_str("      └─\n");
}

fn write_silent_failure(out: &mut String, test_exit_code: i32) {
    out.push('\n');
    out.push_str(SEPARATOR_LIGHT);
    out.push('\n');
    out.push('\n');
    out.push_str(&format!(
        "  ⚠️  Test runner exited with an error (exit code: {test_exit_code})\n"
    ));
    out.push_str("      but no test failures appear in the JUnit report.\n");
    out.push_str("      The report may be incomplete — check your test runner logs.\n");
    out.push('\n');
    out.push_str(SEPARATOR);
    out.push('\n');
    out.push_str("❌ FAIL — test runner exited with an error but no failures were reported\n");
    out.push_str(&format!(
        "  Exit code: {code}\n",
        code = ExitCode::GenericError.as_u8()
    ));
    out.push_str(SEPARATOR);
    out.push('\n');
}

fn write_verdict(out: &mut String, failure_message: Option<&str>, nb_quarantined_failures: usize) {
    out.push('\n');
    out.push_str(SEPARATOR);
    out.push('\n');
    if let Some(msg) = failure_message {
        out.push_str(&format!("❌ FAIL — {msg}\n"));
        out.push_str("  Exit code: 1\n");
    } else if nb_quarantined_failures == 0 {
        out.push_str("✅ OK — all tests passed, no quarantine needed\n");
        out.push_str("  Exit code: 0\n");
    } else {
        let n = nb_quarantined_failures;
        out.push_str(&format!(
            "✅ OK — {n}/{n} failures quarantined, CI status unaffected\n",
        ));
        out.push_str("  Exit code: 0\n");
    }
    out.push_str(SEPARATOR);
    out.push('\n');
}

fn write_early_exit(out: &mut String, message: &str, hint: &str) {
    out.push_str(&format!("❌ FAIL — {message}\n"));
    out.push_str(&format!("  {hint}\n"));
    out.push_str("  Exit code: 1\n");
    out.push_str(SEPARATOR);
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::junit_process::junit::{Failure, TestStatus};
    use std::time::Duration;

    fn case(name: &str, status: TestStatus) -> TestCase {
        TestCase {
            name: name.to_string(),
            suite_name: "s".to_string(),
            duration: Some(Duration::from_secs(0)),
            file: None,
            line: None,
            status,
            failure: Failure::default(),
        }
    }

    #[test]
    fn quarantine_failure_message_signals_blocking_when_check_errored() {
        let result = QuarantineResult {
            failing: vec![case("a", TestStatus::Failed), case("b", TestStatus::Failed)],
            non_quarantined: vec![case("a", TestStatus::Failed), case("b", TestStatus::Failed)],
            quarantined: vec![],
            failing_not_quarantined_count: 2,
        };
        let msg = quarantine_failure_message(&result, 2, true);
        // Pythonic phrasing: "Treating X/X failures as blocking".
        assert_eq!(msg.as_deref(), Some("Treating 2/2 failures as blocking"));
    }

    #[test]
    fn quarantine_failure_message_says_quarantined_when_some_pass() {
        let result = QuarantineResult {
            failing: vec![case("a", TestStatus::Failed), case("b", TestStatus::Failed)],
            quarantined: vec![case("a", TestStatus::Failed)],
            non_quarantined: vec![case("b", TestStatus::Failed)],
            failing_not_quarantined_count: 1,
        };
        // 1/2 still blocking → message says "1/2 quarantined".
        let msg = quarantine_failure_message(&result, 2, false);
        assert_eq!(msg.as_deref(), Some("1/2 failures quarantined"));
    }

    #[test]
    fn quarantine_failure_message_none_when_all_quarantined() {
        let result = QuarantineResult {
            failing: vec![case("a", TestStatus::Failed)],
            quarantined: vec![case("a", TestStatus::Failed)],
            non_quarantined: vec![],
            failing_not_quarantined_count: 0,
        };
        // Every failure quarantined → no failure message.
        assert_eq!(quarantine_failure_message(&result, 1, false), None);
    }

    #[test]
    fn expand_files_rejects_unknown_path() {
        let err = expand_files(&["does/not/exist.xml".to_string()]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("does/not/exist.xml") && msg.contains("does not exist"),
            "got: {msg}"
        );
    }

    #[test]
    fn expand_files_rejects_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("sub");
        std::fs::create_dir(&dir).unwrap();
        let err = expand_files(&[dir.to_string_lossy().to_string()]).unwrap_err();
        assert!(err.to_string().contains("directory"), "got: {err}");
    }

    #[test]
    fn expand_files_dedupes_repeated_literal_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.xml");
        std::fs::write(&path, b"x").unwrap();
        let raw = path.to_string_lossy().to_string();
        let out = expand_files(&[raw.clone(), raw]).unwrap();
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn expand_files_rejects_pattern_with_no_matches() {
        let tmp = tempfile::tempdir().unwrap();
        // tempdir is empty — a wildcard for *.xml here matches nothing.
        let pattern = tmp.path().join("*.xml").to_string_lossy().to_string();
        let err = expand_files(&[pattern]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("did not match any file"), "got: {msg}");
    }

    #[test]
    fn write_verdict_renders_blocking_failure() {
        let mut s = String::new();
        write_verdict(&mut s, Some("1/2 failures quarantined"), 0);
        // The "Exit code: 1" line is what users grep for in CI logs.
        assert!(s.contains("❌ FAIL — 1/2 failures quarantined"), "{s}");
        assert!(s.contains("Exit code: 1"), "{s}");
    }

    #[test]
    fn write_verdict_renders_all_pass() {
        let mut s = String::new();
        write_verdict(&mut s, None, 0);
        assert!(s.contains("✅ OK — all tests passed"), "{s}");
        assert!(s.contains("Exit code: 0"), "{s}");
    }

    #[test]
    fn write_verdict_renders_all_quarantined_pass() {
        let mut s = String::new();
        write_verdict(&mut s, None, 3);
        // "3/3 failures quarantined" — the second arm of the verdict.
        assert!(s.contains("3/3 failures quarantined"), "{s}");
        assert!(s.contains("Exit code: 0"), "{s}");
    }

    #[test]
    fn write_failure_block_handles_missing_details() {
        let mut s = String::new();
        write_failure_block(&mut s, &case("orphan", TestStatus::Failed));
        assert!(s.contains("(no error details in JUnit report)"), "{s}");
    }

    #[test]
    fn write_failure_block_joins_kind_and_message() {
        let mut s = String::new();
        let mut c = case("t", TestStatus::Failed);
        c.failure.kind = Some("AssertionError".to_string());
        c.failure.message = Some("assert 1 == 0".to_string());
        c.failure.stacktrace = Some("line1\nline2".to_string());
        write_failure_block(&mut s, &c);
        // Kind and message joined with ": ", stacktrace lines each
        // prefixed with the box-drawing column.
        assert!(s.contains("AssertionError: assert 1 == 0"), "{s}");
        assert!(s.contains("│  line1"), "{s}");
        assert!(s.contains("│  line2"), "{s}");
    }
}
