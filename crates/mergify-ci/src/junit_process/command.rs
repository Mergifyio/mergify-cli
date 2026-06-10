//! `mergify ci junit-process` orchestration.
//!
//! Glues the four `junit_process` modules together: parse `JUnit`
//! XML → check quarantine status → build OTLP spans (now tagged
//! with `cicd.test.quarantined`) → upload them, then render the
//! human-facing report the way Python's `process_junit_files`
//! does. Errors during quarantine or upload are *non-fatal* by
//! design — Mergify-side trouble must never break customer CI, so
//! the exit code is driven solely by the
//! failing-tests-not-quarantined count plus the silent-failure
//! detection. Upload failures are surfaced instead of swallowed
//! (issue #1571): the report calls them out, and on GitHub
//! Actions the run emits an `::error::`/`::warning::` annotation
//! plus a `test_results_upload` step output so workflows can
//! detect dead ingest programmatically.

// The report builder appends formatted snippets to a single
// `String`. clippy's `format_push_string` lint suggests `write!`
// everywhere, which adds a `use std::fmt::Write` and an awkward
// `let _ = write!(…)` per line for no semantic improvement —
// `String::push_str(&format!(…))` is the readable form for this
// kind of templated text emission.
#![allow(clippy::format_push_string)]

use std::path::{Path, PathBuf};

use mergify_core::env::var_non_empty;
use mergify_core::{CliError, ExitCode, Output};
use url::Url;

use crate::detector;
use crate::junit_process::junit::{self, ParseResult, TestCase};
use crate::junit_process::quarantine::{self, QuarantineFailed, QuarantineResult};
use crate::junit_process::spans::{self, UploadMetadata};
use crate::junit_process::upload;

const SEPARATOR: &str = "══════════════════════════════════════════";
const SEPARATOR_LIGHT: &str = "──────────────────────────────────────────";

/// CLI options for `mergify ci junit-process`.
pub struct JunitProcessOptions<'a> {
    pub api_url: Option<&'a str>,
    pub token: Option<&'a str>,
    pub repository: Option<&'a str>,
    pub test_framework: Option<&'a str>,
    pub test_language: Option<&'a str>,
    pub tests_target_branch: Option<&'a str>,
    pub test_exit_code: Option<i32>,
    /// Raw `files` arguments as the user typed them. Globs (`**`,
    /// `*`, `?`) are expanded here.
    pub files: &'a [String],
}

/// Run the command. Returns an [`ExitCode`] reflecting the final
/// verdict so the caller can plumb it through to the process
/// exit. Network failures (quarantine / upload) do NOT propagate
/// as errors — they print to the report (and annotate on GitHub
/// Actions) and the run continues. The only `Err` paths are
/// argument resolution failures (e.g. missing token) and
/// unrecoverable input errors (no XML, parse failure on every
/// file).
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
    let repository = detector::resolve_repository(opts.repository)?;
    let tests_target_branch = resolve_tests_target_branch(opts.tests_target_branch)?;
    let test_exit_code = resolve_test_exit_code(opts.test_exit_code)?;
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
        mergify_test_job_name: var_non_empty("MERGIFY_TEST_JOB_NAME"),
        quarantined: quarantine_result
            .quarantined
            .iter()
            .map(|c| c.name.clone())
            .collect(),
    };
    let built = spans::build_traces(&parsed, &metadata);

    let client = upload::default_client();
    let upload_error = upload::upload(&client, &api_url_raw, &token, &repository, &built.request)
        .await
        .err();
    maybe_write_github_output(upload_status_label(upload_error.as_ref()));

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
        write_upload_error_block(&mut report, &err.to_string(), err.is_rejection());
        if let Some(annotation) = gha_upload_annotation(err) {
            report.push_str(&annotation);
            report.push('\n');
        }
    }

    write_quarantine_section(&mut report, &quarantine_result, quarantine_error.as_deref());

    // ── Silent-failure detection. If the test runner exited
    // non-zero but the JUnit report has no failures, the runner
    // probably crashed — fail loudly so the user knows the report
    // is incomplete.
    if let Some(exit_code) = test_exit_code {
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
    explicit
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .or_else(|| var_non_empty("MERGIFY_API_URL"))
        .unwrap_or_else(|| "https://api.mergify.com".to_string())
}

/// Resolve `--test-exit-code` / `MERGIFY_TEST_EXIT_CODE`. Empty
/// env value is treated as "unset" so callers like the
/// `gha-mergify-ci` action can export `MERGIFY_TEST_EXIT_CODE=""`
/// to mean "no exit code available" without tripping a clap
/// parse error — the env-var resolution intentionally does not
/// go through clap's `env = ...` attribute, see the
/// `JunitProcessCliArgs::test_exit_code` field for the full
/// rationale.
fn resolve_test_exit_code(explicit: Option<i32>) -> Result<Option<i32>, CliError> {
    if explicit.is_some() {
        return Ok(explicit);
    }
    let Some(raw) = var_non_empty("MERGIFY_TEST_EXIT_CODE") else {
        return Ok(None);
    };
    raw.parse::<i32>().map(Some).map_err(|e| {
        CliError::Configuration(format!(
            "MERGIFY_TEST_EXIT_CODE={raw:?} is not a valid integer: {e}",
        ))
    })
}

fn resolve_token(explicit: Option<&str>) -> Result<String, CliError> {
    explicit
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .or_else(|| var_non_empty("MERGIFY_TOKEN"))
        .ok_or_else(|| {
            CliError::Configuration(
                "--token not provided and MERGIFY_TOKEN env var is empty".to_string(),
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
    // suite span downstream. `suite_names` from each file is
    // appended in order; the OTLP span builder dedupes via its
    // `group_by_suite` linear scan.
    let mut all_cases = Vec::new();
    let mut all_suite_names = Vec::new();
    for path in files {
        let bytes = std::fs::read(path).map_err(|e| junit::InvalidJunitXml {
            details: format!("cannot read {}: {e}", path.display()),
        })?;
        let parsed = junit::parse(&bytes)?;
        all_suite_names.extend(parsed.suite_names);
        all_cases.extend(parsed.cases);
    }
    Ok(ParseResult {
        suite_names: all_suite_names,
        cases: all_cases,
    })
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

/// `$GITHUB_OUTPUT` key reporting the upload outcome:
/// `success`, `rejected` (4xx except 408/429 — permanent, e.g.
/// bad token), or `failed` (transient: 5xx, 408, 429, network).
/// Lets workflows detect dead ingest programmatically without
/// parsing the human report (issue #1571).
const UPLOAD_STATUS_OUTPUT_NAME: &str = "test_results_upload";

fn upload_status_label(error: Option<&upload::UploadError>) -> &'static str {
    match error {
        None => "success",
        Some(e) if e.is_rejection() => "rejected",
        Some(_) => "failed",
    }
}

/// Append `test_results_upload=<status>` to `$GITHUB_OUTPUT` when
/// the env var is set (GitHub Actions). Best effort — reporting
/// plumbing must never break the run, so an unwritable file warns
/// on stderr instead of erroring.
fn maybe_write_github_output(status: &str) {
    use std::io::Write as _;
    let Some(path) = std::env::var("GITHUB_OUTPUT")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        return;
    };
    let result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| writeln!(f, "{UPLOAD_STATUS_OUTPUT_NAME}={status}"));
    if let Err(e) = result {
        eprintln!(
            "warning: could not write {UPLOAD_STATUS_OUTPUT_NAME} to GITHUB_OUTPUT ({path}): {e}",
        );
    }
}

/// GitHub Actions workflow-command annotation for an upload
/// failure, or `None` outside GitHub Actions. The runner parses
/// the `::error::`/`::warning::` line out of stdout and surfaces
/// it in the run summary and checks UI without affecting the step
/// outcome — upload trouble must stay visible but never break
/// customer CI (issue #1571). Rejections (4xx except 408/429, see
/// [`upload::UploadError::is_rejection`]) annotate as errors since
/// they're permanent misconfiguration; transient failures (5xx,
/// 408, 429, network) as warnings.
fn gha_upload_annotation(error: &upload::UploadError) -> Option<String> {
    if std::env::var("GITHUB_ACTIONS").as_deref() != Ok("true") {
        return None;
    }
    Some(if error.is_rejection() {
        let status = error.status.expect("a rejection always has an HTTP status");
        format!(
            "::error title=Mergify Test Insights::Test results upload rejected (HTTP {status}). \
             No test data reached Test Insights — check that your token has CI Insights access \
             to this repository."
        )
    } else {
        format!(
            "::warning title=Mergify Test Insights::Failed to upload test results to Mergify \
             Test Insights ({err}). Test data for this run was not recorded.",
            err = gha_escape_data(&error.to_string()),
        )
    })
}

/// Escape the data section of a GHA workflow command. A multi-line
/// value (e.g. an HTTP response body inside the error message)
/// would otherwise terminate the command at the first newline.
fn gha_escape_data(s: &str) -> String {
    s.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

fn write_upload_error_block(out: &mut String, error: &str, rejected: bool) {
    out.push_str("\n  ⚠️ Failed to upload test results\n");
    out.push_str("    Mergify CI Insights won't process these test results.\n");
    out.push_str("    Quarantine status and CI outcome are unaffected.\n");
    if rejected {
        out.push_str("    The API rejected the upload — check that your token has\n");
        out.push_str("    CI Insights access to this repository.\n");
    }
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
    fn resolve_test_exit_code_returns_explicit_value_when_provided() {
        // Explicit `--test-exit-code 0` must win over whatever the
        // env var says — including the empty-string sentinel the
        // `gha-mergify-ci` action uses when no runner exit code is
        // available. Pin so a future refactor can't accidentally
        // invert the precedence.
        let got = temp_env::with_var("MERGIFY_TEST_EXIT_CODE", Some("42"), || {
            resolve_test_exit_code(Some(0)).unwrap()
        });
        assert_eq!(got, Some(0));
    }

    #[test]
    fn resolve_test_exit_code_treats_empty_env_var_as_unset() {
        // Regression for the downstream `gha-mergify-ci` break
        // (monorepo#33423, second symptom): the action exports
        // `MERGIFY_TEST_EXIT_CODE=""` when the previous step
        // didn't produce a runner exit code. Previously the clap
        // `env = "MERGIFY_TEST_EXIT_CODE"` attribute on
        // `--test-exit-code` tried to parse the empty value as
        // `i32` and aborted parsing with `cannot parse integer
        // from empty string`, before this function ever ran. The
        // fix drops the clap `env` hook and routes the env var
        // through here — empty must collapse to `None`, the
        // same shape no env var would produce.
        let got = temp_env::with_var("MERGIFY_TEST_EXIT_CODE", Some(""), || {
            resolve_test_exit_code(None).unwrap()
        });
        assert_eq!(got, None);
    }

    #[test]
    fn resolve_test_exit_code_parses_non_empty_env_var() {
        let got = temp_env::with_var("MERGIFY_TEST_EXIT_CODE", Some("7"), || {
            resolve_test_exit_code(None).unwrap()
        });
        assert_eq!(got, Some(7));
    }

    #[test]
    fn resolve_test_exit_code_errors_when_env_var_is_non_empty_garbage() {
        // A non-empty env var that isn't a valid integer is a
        // real misconfiguration, not a "no value" sentinel —
        // error loudly with the offending value in the message so
        // the user can spot the typo without having to dig.
        let err = temp_env::with_var("MERGIFY_TEST_EXIT_CODE", Some("not-an-int"), || {
            resolve_test_exit_code(None).unwrap_err()
        });
        let msg = err.to_string();
        assert!(msg.contains("MERGIFY_TEST_EXIT_CODE="), "got: {msg}");
        assert!(msg.contains("not-an-int"), "got: {msg}");
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

    // ── End-to-end orchestrator tests. Drive the full `run()`
    // entry point with on-disk fixtures and a wiremock-backed API
    // so the stdout banner + exit code are pinned for each verdict
    // branch (all-pass, mixed failures, empty input). Cheap to run
    // — wiremock starts a server in-process and the fixtures fit
    // in a string literal each — but they're the only thing that
    // catches a regression in the orchestrator's wiring (e.g. the
    // wrong verdict picked, the early-exit branch mis-routed, the
    // banner text drifting).
    mod orchestrator {
        use super::*;
        use crate::testing::with_ci_env_async;
        use mergify_core::{OutputMode, StdioOutput};
        use std::sync::{Arc, Mutex};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        type SharedBytes = Arc<Mutex<Vec<u8>>>;
        struct SharedWriter(SharedBytes);
        impl std::io::Write for SharedWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        struct Captured {
            output: StdioOutput,
            stdout: SharedBytes,
        }

        fn captured() -> Captured {
            let stdout: SharedBytes = Arc::new(Mutex::new(Vec::new()));
            let stderr: SharedBytes = Arc::new(Mutex::new(Vec::new()));
            let output = StdioOutput::with_sinks(
                OutputMode::Human,
                SharedWriter(Arc::clone(&stdout)),
                SharedWriter(stderr),
            );
            Captured { output, stdout }
        }

        fn write_xml(dir: &tempfile::TempDir, name: &str, xml: &str) -> String {
            let p = dir.path().join(name);
            std::fs::write(&p, xml).unwrap();
            p.to_string_lossy().into_owned()
        }

        // Mount permissive quarantine + upload mocks. Tests assert
        // on stdout/exit-code rather than wire-level mock counts —
        // the wire shape is already covered by `quarantine.rs` and
        // `upload.rs` unit tests.
        async fn mount_mocks(server: &MockServer) {
            Mock::given(method("POST"))
                .and(path("/v1/ci/owner/repositories/repo/quarantines/check"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "quarantined_tests_names": [],
                    "non_quarantined_tests_names": [],
                })))
                .mount(server)
                .await;
            Mock::given(method("POST"))
                .and(path("/v1/repos/owner/repo/ci/traces"))
                .respond_with(ResponseTemplate::new(200))
                .mount(server)
                .await;
        }

        #[tokio::test]
        async fn all_pass_exits_zero_with_ok_banner() {
            let server = MockServer::start().await;
            mount_mocks(&server).await;
            let tmp = tempfile::tempdir().unwrap();
            let file = write_xml(
                &tmp,
                "report.xml",
                r#"<?xml version="1.0"?>
<testsuites>
  <testsuite name="pytest" tests="2" failures="0">
    <testcase classname="tests" name="test_one"/>
    <testcase classname="tests" name="test_two"/>
  </testsuite>
</testsuites>"#,
            );

            let api_url = server.uri();
            let mut cap = captured();
            let code = with_ci_env_async(&[], async {
                let opts = JunitProcessOptions {
                    api_url: Some(&api_url),
                    token: Some("secret"),
                    repository: Some("owner/repo"),
                    test_framework: None,
                    test_language: None,
                    tests_target_branch: Some("main"),
                    test_exit_code: None,
                    files: &[file],
                };
                run(opts, &mut cap.output).await.unwrap()
            })
            .await;

            assert_eq!(code, ExitCode::Success);
            let stdout = String::from_utf8(cap.stdout.lock().unwrap().clone()).unwrap();
            assert!(stdout.contains("✅ OK — all tests passed"), "{stdout}");
            assert!(stdout.contains("Exit code: 0"), "{stdout}");
        }

        #[tokio::test]
        async fn unquarantined_failure_exits_one_with_fail_banner() {
            let server = MockServer::start().await;
            mount_mocks(&server).await;
            let tmp = tempfile::tempdir().unwrap();
            let file = write_xml(
                &tmp,
                "report.xml",
                r#"<?xml version="1.0"?>
<testsuites>
  <testsuite name="pytest" tests="2" failures="1">
    <testcase classname="tests" name="test_success"/>
    <testcase classname="tests" name="test_broken">
      <failure message="assert 1 == 0">stack trace body</failure>
    </testcase>
  </testsuite>
</testsuites>"#,
            );

            let api_url = server.uri();
            let mut cap = captured();
            let code = with_ci_env_async(&[], async {
                let opts = JunitProcessOptions {
                    api_url: Some(&api_url),
                    token: Some("secret"),
                    repository: Some("owner/repo"),
                    test_framework: None,
                    test_language: None,
                    tests_target_branch: Some("main"),
                    test_exit_code: None,
                    files: &[file],
                };
                run(opts, &mut cap.output).await.unwrap()
            })
            .await;

            assert_eq!(code, ExitCode::GenericError);
            let stdout = String::from_utf8(cap.stdout.lock().unwrap().clone()).unwrap();
            // Verdict line: 1 failing test, 0 quarantined.
            assert!(
                stdout.contains("❌ FAIL — 0/1 failures quarantined"),
                "{stdout}"
            );
            assert!(stdout.contains("Exit code: 1"), "{stdout}");
        }

        // Mount a quarantine mock that says "nothing quarantined"
        // and a traces mock that answers `upload_status`.
        async fn mount_mocks_with_upload_status(server: &MockServer, upload_status: u16) {
            Mock::given(method("POST"))
                .and(path("/v1/ci/owner/repositories/repo/quarantines/check"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "quarantined_tests_names": [],
                    "non_quarantined_tests_names": [],
                })))
                .mount(server)
                .await;
            Mock::given(method("POST"))
                .and(path("/v1/repos/owner/repo/ci/traces"))
                .respond_with(
                    ResponseTemplate::new(upload_status).set_body_string("upload refused"),
                )
                .mount(server)
                .await;
        }

        const ONE_PASSING_TEST_XML: &str = r#"<?xml version="1.0"?>
<testsuites>
  <testsuite name="pytest" tests="1" failures="0">
    <testcase classname="tests" name="test_one"/>
  </testsuite>
</testsuites>"#;

        async fn run_with_gha_env(
            api_url: &str,
            file: String,
            github_output: &std::path::Path,
        ) -> (ExitCode, String) {
            let mut cap = captured();
            let output_path = github_output.to_string_lossy().into_owned();
            let code = with_ci_env_async(
                &[
                    ("GITHUB_ACTIONS", Some("true")),
                    ("GITHUB_OUTPUT", Some(&output_path)),
                ],
                async {
                    let opts = JunitProcessOptions {
                        api_url: Some(api_url),
                        token: Some("secret"),
                        repository: Some("owner/repo"),
                        test_framework: None,
                        test_language: None,
                        tests_target_branch: Some("main"),
                        test_exit_code: None,
                        files: &[file],
                    };
                    run(opts, &mut cap.output).await.unwrap()
                },
            )
            .await;
            let stdout = String::from_utf8(cap.stdout.lock().unwrap().clone()).unwrap();
            (code, stdout)
        }

        // Issue #1571: a rejected upload (e.g. token without ingest
        // permission → 403) must stay visible without failing the
        // run — upload trouble must never break customer CI, even
        // when it's a permanent misconfiguration. On GitHub Actions
        // the run surfaces it as an `::error::` annotation plus a
        // `test_results_upload=rejected` step output so workflows
        // can detect dead ingest programmatically.
        #[tokio::test]
        async fn rejected_upload_keeps_run_green_and_annotates() {
            let server = MockServer::start().await;
            mount_mocks_with_upload_status(&server, 403).await;
            let tmp = tempfile::tempdir().unwrap();
            let file = write_xml(&tmp, "report.xml", ONE_PASSING_TEST_XML);
            let github_output = tmp.path().join("github_output");

            let (code, stdout) = run_with_gha_env(&server.uri(), file, &github_output).await;

            assert_eq!(code, ExitCode::Success);
            assert!(stdout.contains("✅ OK — all tests passed"), "{stdout}");
            assert!(stdout.contains("Exit code: 0"), "{stdout}");
            assert!(
                stdout.contains("⚠️ Failed to upload test results"),
                "{stdout}"
            );
            assert!(
                stdout.contains(
                    "::error title=Mergify Test Insights::Test results upload rejected (HTTP 403)"
                ),
                "{stdout}"
            );
            let outputs = std::fs::read_to_string(&github_output).unwrap();
            assert!(
                outputs.contains("test_results_upload=rejected"),
                "{outputs}"
            );
        }

        // Transient backend trouble (5xx, network) keeps the
        // best-effort behavior too, but annotates as a warning and
        // reports `test_results_upload=failed`.
        #[tokio::test]
        async fn transient_upload_error_keeps_run_green_and_warns() {
            let server = MockServer::start().await;
            mount_mocks_with_upload_status(&server, 503).await;
            let tmp = tempfile::tempdir().unwrap();
            let file = write_xml(&tmp, "report.xml", ONE_PASSING_TEST_XML);
            let github_output = tmp.path().join("github_output");

            let (code, stdout) = run_with_gha_env(&server.uri(), file, &github_output).await;

            assert_eq!(code, ExitCode::Success);
            assert!(stdout.contains("✅ OK — all tests passed"), "{stdout}");
            assert!(
                stdout.contains("⚠️ Failed to upload test results"),
                "{stdout}"
            );
            assert!(
                stdout.contains("::warning title=Mergify Test Insights::Failed to upload"),
                "{stdout}"
            );
            let outputs = std::fs::read_to_string(&github_output).unwrap();
            assert!(outputs.contains("test_results_upload=failed"), "{outputs}");
        }

        // A successful upload reports `test_results_upload=success`
        // so workflows get a stable key to branch on, and emits no
        // annotation.
        #[tokio::test]
        async fn successful_upload_writes_success_output_and_no_annotation() {
            let server = MockServer::start().await;
            mount_mocks(&server).await;
            let tmp = tempfile::tempdir().unwrap();
            let file = write_xml(&tmp, "report.xml", ONE_PASSING_TEST_XML);
            let github_output = tmp.path().join("github_output");

            let (code, stdout) = run_with_gha_env(&server.uri(), file, &github_output).await;

            assert_eq!(code, ExitCode::Success);
            assert!(!stdout.contains("::error"), "{stdout}");
            assert!(!stdout.contains("::warning"), "{stdout}");
            let outputs = std::fs::read_to_string(&github_output).unwrap();
            assert!(outputs.contains("test_results_upload=success"), "{outputs}");
        }

        // Outside GitHub Actions there is no workflow-command
        // protocol — the report must stay free of `::error::` noise
        // and no GITHUB_OUTPUT file appears.
        #[tokio::test]
        async fn rejected_upload_outside_gha_emits_no_annotation() {
            let server = MockServer::start().await;
            mount_mocks_with_upload_status(&server, 403).await;
            let tmp = tempfile::tempdir().unwrap();
            let file = write_xml(&tmp, "report.xml", ONE_PASSING_TEST_XML);

            let api_url = server.uri();
            let mut cap = captured();
            let code = with_ci_env_async(&[], async {
                let opts = JunitProcessOptions {
                    api_url: Some(&api_url),
                    token: Some("secret"),
                    repository: Some("owner/repo"),
                    test_framework: None,
                    test_language: None,
                    tests_target_branch: Some("main"),
                    test_exit_code: None,
                    files: &[file],
                };
                run(opts, &mut cap.output).await.unwrap()
            })
            .await;

            assert_eq!(code, ExitCode::Success);
            let stdout = String::from_utf8(cap.stdout.lock().unwrap().clone()).unwrap();
            assert!(
                stdout.contains("⚠️ Failed to upload test results"),
                "{stdout}"
            );
            assert!(!stdout.contains("::error"), "{stdout}");
        }

        #[tokio::test]
        async fn testsuite_without_cases_short_circuits_with_no_spans_error() {
            // A `<testsuites>` wrapping an empty `<testsuite>` is
            // valid XML and gets past the parser (the parser only
            // rejects bare `<testsuites/>` with zero suites), but
            // produces zero TestCase records. The orchestrator must
            // bail out with a specific error before reaching the
            // quarantine + upload layers.
            let tmp = tempfile::tempdir().unwrap();
            let file = write_xml(
                &tmp,
                "report.xml",
                r#"<?xml version="1.0"?>
<testsuites>
  <testsuite name="empty" tests="0"></testsuite>
</testsuites>"#,
            );

            let mut cap = captured();
            // No mock server: if the orchestrator skips the early
            // exit and tries to reach the API, the bogus URL will
            // fail the test loudly.
            let code = with_ci_env_async(&[], async {
                let opts = JunitProcessOptions {
                    api_url: Some("http://127.0.0.1:1"),
                    token: Some("secret"),
                    repository: Some("owner/repo"),
                    test_framework: None,
                    test_language: None,
                    tests_target_branch: Some("main"),
                    test_exit_code: None,
                    files: &[file],
                };
                run(opts, &mut cap.output).await.unwrap()
            })
            .await;

            assert_eq!(code, ExitCode::GenericError);
            let stdout = String::from_utf8(cap.stdout.lock().unwrap().clone()).unwrap();
            assert!(
                stdout.contains("No spans found in the JUnit files"),
                "{stdout}"
            );
        }
    }
}
