import sys

import click
import opentelemetry.trace

from mergify_cli.ci.junit_processing import junit
from mergify_cli.ci.junit_processing import quarantine
from mergify_cli.ci.junit_processing import upload


async def process_junit_files(  # noqa: PLR0913
    *,
    api_url: str,
    token: str,
    repository: str,
    test_framework: str | None,
    test_language: str | None,
    tests_target_branch: str,
    files: tuple[str, ...],
) -> None:
    click.echo("🚀 CI Insights · Upload JUnit")
    click.echo("────────────────────────────")
    click.echo(f"📂 Discovered reports: {len(files)}")

    try:
        spans = await junit.files_to_spans(
            files,
            test_language=test_language,
            test_framework=test_framework,
        )
    except junit.InvalidJunitXMLError as e:
        click.echo(
            click.style(
                f"❌ Error converting JUnit XML file to spans: {e.details}",
                fg="red",
            ),
            err=True,
        )
        sys.exit(1)

    if not spans:
        click.echo(
            click.style("❌ No spans found in the JUnit files", fg="red"),
            err=True,
        )
        sys.exit(1)

    tests_cases = [
        span
        for span in spans
        if span.attributes is not None and span.attributes.get("test.scope") == "case"
    ]

    if not tests_cases:
        click.echo(
            click.style("❌ No test cases found in the JUnit files", fg="red"),
            err=True,
        )
        sys.exit(1)

    nb_failing_spans = len(
        [
            span
            for span in tests_cases
            if span.status.status_code == opentelemetry.trace.StatusCode.ERROR
        ],
    )
    nb_success_spans = len(
        [
            span
            for span in tests_cases
            if span.status.status_code == opentelemetry.trace.StatusCode.OK
        ],
    )
    click.echo(
        f"🧪 Parsed tests: {len(tests_cases)} (✅ passed: {nb_success_spans} | ❌ failed: {nb_failing_spans})",
    )

    # NOTE: Check quarantine before uploading in order to properly modify the
    # "cicd.test.quarantined" attribute for the required spans.
    quarantine_final_failure_message: str | None = None
    try:
        failing_tests_not_quarantined_count = (
            await quarantine.check_and_update_failing_spans(
                api_url,
                token,
                repository,
                tests_target_branch,
                spans,
            )
        )
    except quarantine.QuarantineFailedError as exc:
        click.echo(click.style(exc.message, fg="red"), err=True)
        click.echo(
            click.style(quarantine.QUARANTINE_INFO_ERROR_MSG, fg="red"),
            err=True,
        )
        quarantine_final_failure_message = (
            "Unable to determine quarantined failures due to above error"
        )
    except Exception as exc:  # noqa: BLE001
        msg = (
            f"❌ An unexpected error occurred when checking quarantined tests: {exc!s}"
        )
        click.echo(click.style(msg, fg="red"), err=True)
        click.echo(
            click.style(quarantine.QUARANTINE_INFO_ERROR_MSG, fg="red"),
            err=True,
        )
        quarantine_final_failure_message = (
            "Unable to determine quarantined failures due to above error"
        )
    else:
        if failing_tests_not_quarantined_count > 0:
            quarantine_final_failure_message = f"{failing_tests_not_quarantined_count} unquarantined failures detected ({nb_failing_spans - failing_tests_not_quarantined_count} quarantined)"

    try:
        upload.upload(
            api_url=api_url,
            token=token,
            repository=repository,
            spans=spans,
        )
    except Exception as e:  # noqa: BLE001
        click.echo(
            click.style(f"❌ Error uploading JUnit XML reports: {e}", fg="red"),
            err=True,
        )

    if quarantine_final_failure_message is None:
        click.echo("\n🎉 Verdict")
        click.echo(
            f"• Status: ✅ OK — all {nb_failing_spans} failures are quarantined (ignored for CI status)",
        )
        quarantine_exit_error_code = 0
    else:
        click.echo("\n❌ Verdict")
        click.echo(
            f"• Status: 🔴 FAIL — {quarantine_final_failure_message}",
        )
        quarantine_exit_error_code = 1

    click.echo(f"• Exit code: {quarantine_exit_error_code}")
    sys.exit(quarantine_exit_error_code)
