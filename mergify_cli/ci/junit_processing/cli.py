from __future__ import annotations

import sys
import typing

import click
import opentelemetry.trace

from mergify_cli.ci.junit_processing import junit
from mergify_cli.ci.junit_processing import quarantine
from mergify_cli.ci.junit_processing import upload


if typing.TYPE_CHECKING:
    from opentelemetry.sdk.trace import ReadableSpan

SEPARATOR = "══════════════════════════════════════════"
SEPARATOR_LIGHT = "──────────────────────────────────────────"


async def process_junit_files(
    *,
    api_url: str,
    token: str,
    repository: str,
    test_framework: str | None,
    test_language: str | None,
    tests_target_branch: str,
    files: tuple[str, ...],
    test_exit_code: int | None = None,
) -> None:
    # ── Header ──
    click.echo(SEPARATOR)
    click.echo("  🚀 CI Insights")
    click.echo("")
    click.echo(
        "  Uploads JUnit test results to Mergify CI Insights and evaluates\n"
        "  quarantine status for failing tests. This step determines the\n"
        "  final CI status — quarantined failures are ignored.\n"
        "  Learn more: https://docs.mergify.com/ci-insights/quarantine",
    )
    click.echo(SEPARATOR)

    try:
        run_id, spans = await junit.files_to_spans(
            files,
            test_language=test_language,
            test_framework=test_framework,
        )
    except junit.InvalidJunitXMLError as e:
        _print_early_exit_error(
            f"Failed to parse JUnit XML: {e.details}",
            "Check that your test framework is generating valid JUnit XML output.",
        )
        sys.exit(1)

    if not spans:
        _print_early_exit_error(
            "No spans found in the JUnit files",
            "Check that the JUnit XML files are not empty.",
        )
        sys.exit(1)

    tests_cases = [
        span
        for span in spans
        if span.attributes is not None and span.attributes.get("test.scope") == "case"
    ]

    if not tests_cases:
        _print_early_exit_error(
            "No test cases found in the JUnit files",
            "Check that your test step ran successfully before this step.",
        )
        sys.exit(1)

    nb_failing_spans = len(
        [
            span
            for span in tests_cases
            if span.status.status_code == opentelemetry.trace.StatusCode.ERROR
        ],
    )
    click.echo("")
    click.echo(f"  Run ID: {run_id}")

    # NOTE: Check quarantine before uploading in order to properly modify the
    # "cicd.test.quarantined" attribute for the required spans.
    quarantine_final_failure_message: str | None = None
    quarantine_error: str | None = None
    try:
        result = await quarantine.check_and_update_failing_spans(
            api_url,
            token,
            repository,
            tests_target_branch,
            spans,
        )
    except quarantine.QuarantineFailedError as exc:
        quarantine_error = exc.message
        quarantine_final_failure_message = (
            f"Treating {nb_failing_spans}/{nb_failing_spans} failures as blocking"
        )
        failing_spans = [
            span
            for span in tests_cases
            if span.status.status_code == opentelemetry.trace.StatusCode.ERROR
        ]
        result = quarantine.QuarantineResult(
            failing_spans=failing_spans,
            quarantined_spans=[],
            non_quarantined_spans=failing_spans,
            failing_tests_not_quarantined_count=len(failing_spans),
        )
    except Exception as exc:
        quarantine_error = str(exc)
        quarantine_final_failure_message = (
            f"Treating {nb_failing_spans}/{nb_failing_spans} failures as blocking"
        )
        failing_spans = [
            span
            for span in tests_cases
            if span.status.status_code == opentelemetry.trace.StatusCode.ERROR
        ]
        result = quarantine.QuarantineResult(
            failing_spans=failing_spans,
            quarantined_spans=[],
            non_quarantined_spans=failing_spans,
            failing_tests_not_quarantined_count=len(failing_spans),
        )
    else:
        if result.failing_tests_not_quarantined_count > 0:
            count = result.failing_tests_not_quarantined_count
            total = len(result.failing_spans)
            quarantined = total - count
            quarantine_final_failure_message = (
                f"{quarantined}/{total} failures quarantined"
            )

    upload_error: str | None = None
    try:
        upload.upload(
            api_url=api_url,
            token=token,
            repository=repository,
            spans=spans,
        )
    except Exception as e:
        upload_error = str(e)

    reports_label = f"{len(files)} {'report' if len(files) == 1 else 'reports'}"
    failures_label = (
        f"{nb_failing_spans} {'failure' if nb_failing_spans == 1 else 'failures'}"
    )
    if upload_error is not None:
        click.echo(f"      ☁️ {reports_label} not uploaded")
    else:
        click.echo(f"      ☁️ {reports_label} uploaded")
    click.echo(f"      🧪 {len(tests_cases)} tests ({failures_label})")

    # ── Upload error ──
    if upload_error is not None:
        click.echo("\n  ⚠️ Failed to upload test results")
        click.echo("    Mergify CI Insights won't process these test results.")
        click.echo("    Quarantine status and CI outcome are unaffected.")
        click.echo("")
        click.echo("      ┌ Details")
        for line in upload_error.splitlines():
            click.echo(f"      │  {line}")
        click.echo("      └─")

    # ── Quarantine ──
    _print_quarantine_section(result, error=quarantine_error)

    # ── Silent failure detection ──
    if test_exit_code is not None and test_exit_code != 0 and nb_failing_spans == 0:
        click.echo("")
        click.echo(SEPARATOR_LIGHT)
        click.echo("")
        click.echo(
            f"  ⚠️  Test runner exited with an error (exit code: {test_exit_code})",
        )
        click.echo("      but no test failures appear in the JUnit report.")
        click.echo("      The report may be incomplete — check your test runner logs.")
        click.echo("")
        click.echo(SEPARATOR)
        click.echo(
            "❌ FAIL — test runner exited with an error but no failures were reported",
        )
        click.echo("  Exit code: 1")
        click.echo(SEPARATOR)
        sys.exit(1)

    # ── Verdict ──
    nb_quarantined_failures = len(result.failing_spans) if result is not None else 0
    click.echo("")
    click.echo(SEPARATOR)
    if quarantine_final_failure_message is None:
        if nb_quarantined_failures == 0:
            click.echo("✅ OK — all tests passed, no quarantine needed")
        else:
            click.echo(
                f"✅ OK — {nb_quarantined_failures}/{nb_quarantined_failures}"
                " failures quarantined, CI status unaffected",
            )
        quarantine_exit_error_code = 0
    else:
        click.echo(
            f"❌ FAIL — {quarantine_final_failure_message}",
        )
        quarantine_exit_error_code = 1

    click.echo(f"  Exit code: {quarantine_exit_error_code}")
    click.echo(SEPARATOR)
    sys.exit(quarantine_exit_error_code)


def _print_quarantine_section(
    result: quarantine.QuarantineResult | None,
    *,
    error: str | None = None,
) -> bool:
    """Print the quarantine section. Returns True if anything was printed."""
    if result is None and error is None:
        return False

    if result is not None and not result.failing_spans and error is None:
        return False

    click.echo("")
    click.echo(SEPARATOR_LIGHT)
    click.echo("")
    click.echo("🛡️ Quarantine")

    if error is not None:
        click.echo("")
        click.echo("  ⚠️ Failed to check quarantine status")
        click.echo("    Contact Mergify support if this error persists.")
        click.echo("")
        click.echo("      ┌ Details")
        for line in error.splitlines():
            click.echo(f"      │  {line}")
        click.echo("      └─")

    if result is not None and result.quarantined_spans:
        click.echo(f"\n  🔒 Quarantined ({len(result.quarantined_spans)}):")
        for span in result.quarantined_spans:
            click.echo(f"      · {span.name}")

    if result is not None and result.non_quarantined_spans:
        label = (
            "Could not verify quarantine status"
            if error is not None
            else "Unquarantined"
        )
        click.echo(f"\n  ❌ {label} ({len(result.non_quarantined_spans)}):")
        for span in result.non_quarantined_spans:
            _print_failure_block(span)

    return True


def _print_failure_block(span: ReadableSpan) -> None:
    """Print a box-drawn failure block for a single failing test span."""
    click.echo(f"\n      ┌ {span.name}")

    if span.attributes is None:
        click.echo("      │")
        click.echo("      │  (no error details in JUnit report)")
        click.echo("      └─")
        return

    exc_type = span.attributes.get("exception.type")
    exc_message = span.attributes.get("exception.message")
    exc_stacktrace = span.attributes.get("exception.stacktrace")

    if not exc_type and not exc_message and not exc_stacktrace:
        click.echo("      │")
        click.echo("      │  (no error details in JUnit report)")
        click.echo("      └─")
        return

    if exc_type or exc_message:
        parts = []
        if exc_type:
            parts.append(str(exc_type))
        if exc_message:
            parts.append(str(exc_message))
        click.echo("      │")
        click.echo(f"      │  {': '.join(parts)}")

    if exc_stacktrace:
        click.echo("      │")
        for line in str(exc_stacktrace).splitlines():
            click.echo(f"      │  {line}")

    click.echo("      └─")


def _print_early_exit_error(message: str, hint: str) -> None:
    """Print an error section for early exits (invalid XML, no tests, etc.)."""
    click.echo(f"❌ FAIL — {message}")
    click.echo(f"  {hint}")
    click.echo("  Exit code: 1")
    click.echo(SEPARATOR)
