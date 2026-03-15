from __future__ import annotations

import dataclasses
import pathlib
from unittest import mock

import anys
import httpx
from opentelemetry.sdk.trace import ReadableSpan
from opentelemetry.sdk.trace import id_generator
from opentelemetry.trace import Status
from opentelemetry.trace import StatusCode
import pytest

from mergify_cli.ci.junit_processing import cli
from mergify_cli.ci.junit_processing import quarantine
from mergify_cli.ci.junit_processing import upload


FIXTURES_DIR = pathlib.Path(__file__).parent.parent / "fixtures"
REPORT_XML = FIXTURES_DIR / "report.xml"
REPORT_MIXED_XML = FIXTURES_DIR / "report_mixed.xml"
REPORT_INVALID_XML = FIXTURES_DIR / "report_invalid.xml"
REPORT_NO_TESTCASES_XML = FIXTURES_DIR / "report_no_testcases.xml"
REPORT_ALL_PASS_XML = FIXTURES_DIR / "report_all_pass.xml"

FAILING_SPAN = ReadableSpan(
    name="mergify.tests.test_junit.test_failed",
    status=Status(status_code=StatusCode.ERROR, description=""),
    attributes={
        "test.scope": "case",
        "exception.message": "assert 1 == 0",
        "exception.stacktrace": (
            "def test_failed() -> None:\n"
            "                > assert 1 == 0\n"
            "                E assert 1 == 0\n"
            "\n"
            "                mergify/tests/test_junit.py:6: AssertionError"
        ),
    },
)


@dataclasses.dataclass
class ProcessResult:
    exit_code: int
    stdout: str
    stderr: str
    upload_mock: mock.MagicMock


async def _run_process(
    *,
    files: tuple[str, ...] = (str(REPORT_XML),),
    quarantine_result: quarantine.QuarantineResult | None = None,
    quarantine_side_effect: Exception | None = None,
    upload_side_effect: Exception | None = None,
    capsys: pytest.CaptureFixture[str],
) -> ProcessResult:
    quarantine_mock = mock.AsyncMock()
    if quarantine_side_effect is not None:
        quarantine_mock.side_effect = quarantine_side_effect
    elif quarantine_result is not None:
        quarantine_mock.return_value = quarantine_result

    upload_mock = mock.Mock()
    if upload_side_effect is not None:
        upload_mock.side_effect = upload_side_effect

    with (
        mock.patch.object(
            quarantine,
            "check_and_update_failing_spans",
            quarantine_mock,
        ),
        mock.patch.object(
            upload,
            "upload",
            upload_mock,
        ) as mocked_upload,
        mock.patch.object(
            id_generator.RandomIdGenerator,
            "generate_span_id",
            return_value=12345678910,
        ),
        pytest.raises(SystemExit) as exc_info,
    ):
        await cli.process_junit_files(
            api_url="https://api.mergify.com",
            token="foobar",
            repository="foo/bar",
            test_framework=None,
            test_language=None,
            tests_target_branch="main",
            files=files,
        )

    captured = capsys.readouterr()
    assert isinstance(exc_info.value.code, int)
    return ProcessResult(
        exit_code=exc_info.value.code,
        stdout=captured.out,
        stderr=captured.err,
        upload_mock=mocked_upload,
    )


# ── Happy paths ──


async def test_all_failures_quarantined(
    capsys: pytest.CaptureFixture[str],
) -> None:
    result = await _run_process(
        quarantine_result=quarantine.QuarantineResult(
            failing_spans=[FAILING_SPAN],
            quarantined_spans=[FAILING_SPAN],
            non_quarantined_spans=[],
            failing_tests_not_quarantined_count=0,
        ),
        capsys=capsys,
    )

    assert result.exit_code == 0
    assert result.stdout == (
        "══════════════════════════════════════════\n"
        "  🚀 CI Insights\n"
        "\n"
        "  Uploads JUnit test results to Mergify CI Insights and evaluates\n"
        "  quarantine status for failing tests. This step determines the\n"
        "  final CI status — quarantined failures are ignored.\n"
        "  Learn more: https://docs.mergify.com/ci-insights/quarantine\n"
        "══════════════════════════════════════════\n"
        "\n"
        "  Run ID: 00000002dfdc1c3e\n"
        "      ☁️ 1 report uploaded\n"
        "      🧪 2 tests (1 failure)\n"
        "\n"
        "──────────────────────────────────────────\n"
        "\n"
        "🛡️ Quarantine\n"
        "\n"
        "  🔒 Quarantined (1):\n"
        "      · mergify.tests.test_junit.test_failed\n"
        "\n"
        "══════════════════════════════════════════\n"
        "✅ OK — 1/1 failures quarantined, CI status unaffected\n"
        "  Exit code: 0\n"
        "══════════════════════════════════════════\n"
    )


async def test_no_failed_tests(
    capsys: pytest.CaptureFixture[str],
) -> None:
    result = await _run_process(
        files=(str(REPORT_ALL_PASS_XML),),
        quarantine_result=quarantine.QuarantineResult(
            failing_spans=[],
            quarantined_spans=[],
            non_quarantined_spans=[],
            failing_tests_not_quarantined_count=0,
        ),
        capsys=capsys,
    )

    assert result.exit_code == 0
    assert result.stdout == (
        "══════════════════════════════════════════\n"
        "  🚀 CI Insights\n"
        "\n"
        "  Uploads JUnit test results to Mergify CI Insights and evaluates\n"
        "  quarantine status for failing tests. This step determines the\n"
        "  final CI status — quarantined failures are ignored.\n"
        "  Learn more: https://docs.mergify.com/ci-insights/quarantine\n"
        "══════════════════════════════════════════\n"
        "\n"
        "  Run ID: 00000002dfdc1c3e\n"
        "      ☁️ 1 report uploaded\n"
        "      🧪 2 tests (0 failures)\n"
        "\n"
        "══════════════════════════════════════════\n"
        "✅ OK — all tests passed, no quarantine needed\n"
        "  Exit code: 0\n"
        "══════════════════════════════════════════\n"
    )


async def test_multiple_report_files(
    capsys: pytest.CaptureFixture[str],
) -> None:
    result = await _run_process(
        files=(str(REPORT_ALL_PASS_XML), str(REPORT_ALL_PASS_XML)),
        quarantine_result=quarantine.QuarantineResult(
            failing_spans=[],
            quarantined_spans=[],
            non_quarantined_spans=[],
            failing_tests_not_quarantined_count=0,
        ),
        capsys=capsys,
    )

    assert result.exit_code == 0
    assert result.stdout == (
        "══════════════════════════════════════════\n"
        "  🚀 CI Insights\n"
        "\n"
        "  Uploads JUnit test results to Mergify CI Insights and evaluates\n"
        "  quarantine status for failing tests. This step determines the\n"
        "  final CI status — quarantined failures are ignored.\n"
        "  Learn more: https://docs.mergify.com/ci-insights/quarantine\n"
        "══════════════════════════════════════════\n"
        "\n"
        "  Run ID: 00000002dfdc1c3e\n"
        "      ☁️ 2 reports uploaded\n"
        "      🧪 4 tests (0 failures)\n"
        "\n"
        "══════════════════════════════════════════\n"
        "✅ OK — all tests passed, no quarantine needed\n"
        "  Exit code: 0\n"
        "══════════════════════════════════════════\n"
    )


# ── Unquarantined failures ──


async def test_unquarantined_failure_with_stacktrace(
    capsys: pytest.CaptureFixture[str],
) -> None:
    result = await _run_process(
        quarantine_result=quarantine.QuarantineResult(
            failing_spans=[FAILING_SPAN],
            quarantined_spans=[],
            non_quarantined_spans=[FAILING_SPAN],
            failing_tests_not_quarantined_count=1,
        ),
        capsys=capsys,
    )

    assert result.exit_code == 1
    assert result.stdout == (
        "══════════════════════════════════════════\n"
        "  🚀 CI Insights\n"
        "\n"
        "  Uploads JUnit test results to Mergify CI Insights and evaluates\n"
        "  quarantine status for failing tests. This step determines the\n"
        "  final CI status — quarantined failures are ignored.\n"
        "  Learn more: https://docs.mergify.com/ci-insights/quarantine\n"
        "══════════════════════════════════════════\n"
        "\n"
        "  Run ID: 00000002dfdc1c3e\n"
        "      ☁️ 1 report uploaded\n"
        "      🧪 2 tests (1 failure)\n"
        "\n"
        "──────────────────────────────────────────\n"
        "\n"
        "🛡️ Quarantine\n"
        "\n"
        "  ❌ Unquarantined (1):\n"
        "\n"
        "      ┌ mergify.tests.test_junit.test_failed\n"
        "      │\n"
        "      │  assert 1 == 0\n"
        "      │\n"
        "      │  def test_failed() -> None:\n"
        "      │                  > assert 1 == 0\n"
        "      │                  E assert 1 == 0\n"
        "      │  \n"
        "      │                  mergify/tests/test_junit.py:6: AssertionError\n"
        "      └─\n"
        "\n"
        "══════════════════════════════════════════\n"
        "❌ FAIL — 0/1 failures quarantined\n"
        "  Exit code: 1\n"
        "══════════════════════════════════════════\n"
    )


async def test_mixed_quarantined_and_unquarantined(
    capsys: pytest.CaptureFixture[str],
) -> None:
    quarantined_span = ReadableSpan(
        name="tests.test_flaky",
        status=Status(status_code=StatusCode.ERROR, description=""),
        attributes={
            "test.scope": "case",
            "exception.message": "flaky timeout",
            "exception.stacktrace": "Connection timed out",
        },
    )
    unquarantined_span = ReadableSpan(
        name="tests.test_broken",
        status=Status(status_code=StatusCode.ERROR, description=""),
        attributes={
            "test.scope": "case",
            "exception.type": "ValueError",
            "exception.message": "invalid input",
            "exception.stacktrace": (
                'Traceback:\n  File "test.py", line 10\nValueError: invalid input'
            ),
        },
    )

    result = await _run_process(
        files=(str(REPORT_MIXED_XML),),
        quarantine_result=quarantine.QuarantineResult(
            failing_spans=[quarantined_span, unquarantined_span],
            quarantined_spans=[quarantined_span],
            non_quarantined_spans=[unquarantined_span],
            failing_tests_not_quarantined_count=1,
        ),
        capsys=capsys,
    )

    assert result.exit_code == 1
    assert result.stdout == (
        "══════════════════════════════════════════\n"
        "  🚀 CI Insights\n"
        "\n"
        "  Uploads JUnit test results to Mergify CI Insights and evaluates\n"
        "  quarantine status for failing tests. This step determines the\n"
        "  final CI status — quarantined failures are ignored.\n"
        "  Learn more: https://docs.mergify.com/ci-insights/quarantine\n"
        "══════════════════════════════════════════\n"
        "\n"
        "  Run ID: 00000002dfdc1c3e\n"
        "      ☁️ 1 report uploaded\n"
        "      🧪 3 tests (2 failures)\n"
        "\n"
        "──────────────────────────────────────────\n"
        "\n"
        "🛡️ Quarantine\n"
        "\n"
        "  🔒 Quarantined (1):\n"
        "      · tests.test_flaky\n"
        "\n"
        "  ❌ Unquarantined (1):\n"
        "\n"
        "      ┌ tests.test_broken\n"
        "      │\n"
        "      │  ValueError: invalid input\n"
        "      │\n"
        "      │  Traceback:\n"
        '      │    File "test.py", line 10\n'
        "      │  ValueError: invalid input\n"
        "      └─\n"
        "\n"
        "══════════════════════════════════════════\n"
        "❌ FAIL — 1/2 failures quarantined\n"
        "  Exit code: 1\n"
        "══════════════════════════════════════════\n"
    )


async def test_unquarantined_failure_no_exception_attributes(
    capsys: pytest.CaptureFixture[str],
) -> None:
    bare_span = ReadableSpan(
        name="tests.test_bare",
        status=Status(status_code=StatusCode.ERROR, description=""),
        attributes={"test.scope": "case"},
    )

    result = await _run_process(
        quarantine_result=quarantine.QuarantineResult(
            failing_spans=[bare_span],
            quarantined_spans=[],
            non_quarantined_spans=[bare_span],
            failing_tests_not_quarantined_count=1,
        ),
        capsys=capsys,
    )

    assert result.exit_code == 1
    assert result.stdout == (
        "══════════════════════════════════════════\n"
        "  🚀 CI Insights\n"
        "\n"
        "  Uploads JUnit test results to Mergify CI Insights and evaluates\n"
        "  quarantine status for failing tests. This step determines the\n"
        "  final CI status — quarantined failures are ignored.\n"
        "  Learn more: https://docs.mergify.com/ci-insights/quarantine\n"
        "══════════════════════════════════════════\n"
        "\n"
        "  Run ID: 00000002dfdc1c3e\n"
        "      ☁️ 1 report uploaded\n"
        "      🧪 2 tests (1 failure)\n"
        "\n"
        "──────────────────────────────────────────\n"
        "\n"
        "🛡️ Quarantine\n"
        "\n"
        "  ❌ Unquarantined (1):\n"
        "\n"
        "      ┌ tests.test_bare\n"
        "      │\n"
        "      │  (no error details in JUnit report)\n"
        "      └─\n"
        "\n"
        "══════════════════════════════════════════\n"
        "❌ FAIL — 0/1 failures quarantined\n"
        "  Exit code: 1\n"
        "══════════════════════════════════════════\n"
    )


async def test_unquarantined_failure_only_exception_type(
    capsys: pytest.CaptureFixture[str],
) -> None:
    span = ReadableSpan(
        name="tests.test_type_only",
        status=Status(status_code=StatusCode.ERROR, description=""),
        attributes={
            "test.scope": "case",
            "exception.type": "RuntimeError",
        },
    )

    result = await _run_process(
        quarantine_result=quarantine.QuarantineResult(
            failing_spans=[span],
            quarantined_spans=[],
            non_quarantined_spans=[span],
            failing_tests_not_quarantined_count=1,
        ),
        capsys=capsys,
    )

    assert result.exit_code == 1
    assert result.stdout == (
        "══════════════════════════════════════════\n"
        "  🚀 CI Insights\n"
        "\n"
        "  Uploads JUnit test results to Mergify CI Insights and evaluates\n"
        "  quarantine status for failing tests. This step determines the\n"
        "  final CI status — quarantined failures are ignored.\n"
        "  Learn more: https://docs.mergify.com/ci-insights/quarantine\n"
        "══════════════════════════════════════════\n"
        "\n"
        "  Run ID: 00000002dfdc1c3e\n"
        "      ☁️ 1 report uploaded\n"
        "      🧪 2 tests (1 failure)\n"
        "\n"
        "──────────────────────────────────────────\n"
        "\n"
        "🛡️ Quarantine\n"
        "\n"
        "  ❌ Unquarantined (1):\n"
        "\n"
        "      ┌ tests.test_type_only\n"
        "      │\n"
        "      │  RuntimeError\n"
        "      └─\n"
        "\n"
        "══════════════════════════════════════════\n"
        "❌ FAIL — 0/1 failures quarantined\n"
        "  Exit code: 1\n"
        "══════════════════════════════════════════\n"
    )


async def test_unquarantined_failure_with_none_attributes(
    capsys: pytest.CaptureFixture[str],
) -> None:
    span = ReadableSpan(
        name="tests.test_none_attrs",
        status=Status(status_code=StatusCode.ERROR, description=""),
        attributes=None,
    )

    result = await _run_process(
        quarantine_result=quarantine.QuarantineResult(
            failing_spans=[span],
            quarantined_spans=[],
            non_quarantined_spans=[span],
            failing_tests_not_quarantined_count=1,
        ),
        capsys=capsys,
    )

    assert result.exit_code == 1
    assert result.stdout == (
        "══════════════════════════════════════════\n"
        "  🚀 CI Insights\n"
        "\n"
        "  Uploads JUnit test results to Mergify CI Insights and evaluates\n"
        "  quarantine status for failing tests. This step determines the\n"
        "  final CI status — quarantined failures are ignored.\n"
        "  Learn more: https://docs.mergify.com/ci-insights/quarantine\n"
        "══════════════════════════════════════════\n"
        "\n"
        "  Run ID: 00000002dfdc1c3e\n"
        "      ☁️ 1 report uploaded\n"
        "      🧪 2 tests (1 failure)\n"
        "\n"
        "──────────────────────────────────────────\n"
        "\n"
        "🛡️ Quarantine\n"
        "\n"
        "  ❌ Unquarantined (1):\n"
        "\n"
        "      ┌ tests.test_none_attrs\n"
        "      │\n"
        "      │  (no error details in JUnit report)\n"
        "      └─\n"
        "\n"
        "══════════════════════════════════════════\n"
        "❌ FAIL — 0/1 failures quarantined\n"
        "  Exit code: 1\n"
        "══════════════════════════════════════════\n"
    )


# ── Quarantine errors ──


async def test_quarantine_handled_error(
    capsys: pytest.CaptureFixture[str],
) -> None:
    result = await _run_process(
        quarantine_side_effect=quarantine.QuarantineFailedError(
            'HTTP 422: {"detail": "No subscription"}',
        ),
        capsys=capsys,
    )

    assert result.exit_code == 1
    assert result.stdout == (
        "══════════════════════════════════════════\n"
        "  🚀 CI Insights\n"
        "\n"
        "  Uploads JUnit test results to Mergify CI Insights and evaluates\n"
        "  quarantine status for failing tests. This step determines the\n"
        "  final CI status — quarantined failures are ignored.\n"
        "  Learn more: https://docs.mergify.com/ci-insights/quarantine\n"
        "══════════════════════════════════════════\n"
        "\n"
        "  Run ID: 00000002dfdc1c3e\n"
        "      ☁️ 1 report uploaded\n"
        "      🧪 2 tests (1 failure)\n"
        "\n"
        "──────────────────────────────────────────\n"
        "\n"
        "🛡️ Quarantine\n"
        "\n"
        "  ⚠️ Failed to check quarantine status\n"
        "    Contact Mergify support if this error persists.\n"
        "\n"
        "      ┌ Details\n"
        '      │  HTTP 422: {"detail": "No subscription"}\n'
        "      └─\n"
        "\n"
        "  ❌ Could not verify quarantine status (1):\n"
        "\n"
        "      ┌ mergify.tests.test_junit.test_failed\n"
        "      │\n"
        "      │  assert 1 == 0\n"
        "      │\n"
        "      │  def test_failed() -> None:\n"
        "      │                  > assert 1 == 0\n"
        "      │                  E assert 1 == 0\n"
        "      │  \n"
        "      │                  mergify/tests/test_junit.py:6: AssertionError\n"
        "      └─\n"
        "\n"
        "══════════════════════════════════════════\n"
        "❌ FAIL — Treating 1/1 failures as blocking\n"
        "  Exit code: 1\n"
        "══════════════════════════════════════════\n"
    )


async def test_quarantine_unhandled_error(
    capsys: pytest.CaptureFixture[str],
) -> None:
    result = await _run_process(
        quarantine_side_effect=httpx.ConnectError("Connection refused"),
        capsys=capsys,
    )

    assert result.exit_code == 1
    assert result.stdout == (
        "══════════════════════════════════════════\n"
        "  🚀 CI Insights\n"
        "\n"
        "  Uploads JUnit test results to Mergify CI Insights and evaluates\n"
        "  quarantine status for failing tests. This step determines the\n"
        "  final CI status — quarantined failures are ignored.\n"
        "  Learn more: https://docs.mergify.com/ci-insights/quarantine\n"
        "══════════════════════════════════════════\n"
        "\n"
        "  Run ID: 00000002dfdc1c3e\n"
        "      ☁️ 1 report uploaded\n"
        "      🧪 2 tests (1 failure)\n"
        "\n"
        "──────────────────────────────────────────\n"
        "\n"
        "🛡️ Quarantine\n"
        "\n"
        "  ⚠️ Failed to check quarantine status\n"
        "    Contact Mergify support if this error persists.\n"
        "\n"
        "      ┌ Details\n"
        "      │  Connection refused\n"
        "      └─\n"
        "\n"
        "  ❌ Could not verify quarantine status (1):\n"
        "\n"
        "      ┌ mergify.tests.test_junit.test_failed\n"
        "      │\n"
        "      │  assert 1 == 0\n"
        "      │\n"
        "      │  def test_failed() -> None:\n"
        "      │                  > assert 1 == 0\n"
        "      │                  E assert 1 == 0\n"
        "      │  \n"
        "      │                  mergify/tests/test_junit.py:6: AssertionError\n"
        "      └─\n"
        "\n"
        "══════════════════════════════════════════\n"
        "❌ FAIL — Treating 1/1 failures as blocking\n"
        "  Exit code: 1\n"
        "══════════════════════════════════════════\n"
    )


# ── Upload errors ──


async def test_upload_failure(
    capsys: pytest.CaptureFixture[str],
) -> None:
    result = await _run_process(
        files=(str(REPORT_ALL_PASS_XML),),
        quarantine_result=quarantine.QuarantineResult(
            failing_spans=[],
            quarantined_spans=[],
            non_quarantined_spans=[],
            failing_tests_not_quarantined_count=0,
        ),
        upload_side_effect=Exception("Connection refused"),
        capsys=capsys,
    )

    assert result.exit_code == 0
    assert result.upload_mock.call_count == 1
    assert result.upload_mock.call_args.kwargs == {
        "api_url": "https://api.mergify.com",
        "token": "foobar",
        "repository": "foo/bar",
        "spans": anys.ANY_LIST,
    }
    assert result.stdout == (
        "══════════════════════════════════════════\n"
        "  🚀 CI Insights\n"
        "\n"
        "  Uploads JUnit test results to Mergify CI Insights and evaluates\n"
        "  quarantine status for failing tests. This step determines the\n"
        "  final CI status — quarantined failures are ignored.\n"
        "  Learn more: https://docs.mergify.com/ci-insights/quarantine\n"
        "══════════════════════════════════════════\n"
        "\n"
        "  Run ID: 00000002dfdc1c3e\n"
        "      ☁️ 1 report not uploaded\n"
        "      🧪 2 tests (0 failures)\n"
        "\n"
        "  ⚠️ Failed to upload test results\n"
        "    Mergify CI Insights won't process these test results.\n"
        "    Quarantine status and CI outcome are unaffected.\n"
        "\n"
        "      ┌ Details\n"
        "      │  Connection refused\n"
        "      └─\n"
        "\n"
        "══════════════════════════════════════════\n"
        "✅ OK — all tests passed, no quarantine needed\n"
        "  Exit code: 0\n"
        "══════════════════════════════════════════\n"
    )


async def test_upload_failure_with_unquarantined_failures(
    capsys: pytest.CaptureFixture[str],
) -> None:
    result = await _run_process(
        quarantine_result=quarantine.QuarantineResult(
            failing_spans=[FAILING_SPAN],
            quarantined_spans=[],
            non_quarantined_spans=[FAILING_SPAN],
            failing_tests_not_quarantined_count=1,
        ),
        upload_side_effect=Exception("Connection refused"),
        capsys=capsys,
    )

    assert result.exit_code == 1
    assert result.stdout == (
        "══════════════════════════════════════════\n"
        "  🚀 CI Insights\n"
        "\n"
        "  Uploads JUnit test results to Mergify CI Insights and evaluates\n"
        "  quarantine status for failing tests. This step determines the\n"
        "  final CI status — quarantined failures are ignored.\n"
        "  Learn more: https://docs.mergify.com/ci-insights/quarantine\n"
        "══════════════════════════════════════════\n"
        "\n"
        "  Run ID: 00000002dfdc1c3e\n"
        "      ☁️ 1 report not uploaded\n"
        "      🧪 2 tests (1 failure)\n"
        "\n"
        "  ⚠️ Failed to upload test results\n"
        "    Mergify CI Insights won't process these test results.\n"
        "    Quarantine status and CI outcome are unaffected.\n"
        "\n"
        "      ┌ Details\n"
        "      │  Connection refused\n"
        "      └─\n"
        "\n"
        "──────────────────────────────────────────\n"
        "\n"
        "🛡️ Quarantine\n"
        "\n"
        "  ❌ Unquarantined (1):\n"
        "\n"
        "      ┌ mergify.tests.test_junit.test_failed\n"
        "      │\n"
        "      │  assert 1 == 0\n"
        "      │\n"
        "      │  def test_failed() -> None:\n"
        "      │                  > assert 1 == 0\n"
        "      │                  E assert 1 == 0\n"
        "      │  \n"
        "      │                  mergify/tests/test_junit.py:6: AssertionError\n"
        "      └─\n"
        "\n"
        "══════════════════════════════════════════\n"
        "❌ FAIL — 0/1 failures quarantined\n"
        "  Exit code: 1\n"
        "══════════════════════════════════════════\n"
    )


# ── Early exit cases ──


async def test_invalid_junit_xml(
    capsys: pytest.CaptureFixture[str],
) -> None:
    result = await _run_process(
        files=(str(REPORT_INVALID_XML),),
        capsys=capsys,
    )

    assert result.exit_code == 1
    assert result.stdout == (
        "══════════════════════════════════════════\n"
        "  🚀 CI Insights\n"
        "\n"
        "  Uploads JUnit test results to Mergify CI Insights and evaluates\n"
        "  quarantine status for failing tests. This step determines the\n"
        "  final CI status — quarantined failures are ignored.\n"
        "  Learn more: https://docs.mergify.com/ci-insights/quarantine\n"
        "══════════════════════════════════════════\n"
        "❌ FAIL — Failed to parse JUnit XML: syntax error: line 1, column 0\n"
        "  Check that your test framework is generating valid JUnit XML output.\n"
        "  Exit code: 1\n"
        "══════════════════════════════════════════\n"
    )


async def test_no_test_cases_in_junit(
    capsys: pytest.CaptureFixture[str],
) -> None:
    result = await _run_process(
        files=(str(REPORT_NO_TESTCASES_XML),),
        capsys=capsys,
    )

    assert result.exit_code == 1
    assert result.stdout == (
        "══════════════════════════════════════════\n"
        "  🚀 CI Insights\n"
        "\n"
        "  Uploads JUnit test results to Mergify CI Insights and evaluates\n"
        "  quarantine status for failing tests. This step determines the\n"
        "  final CI status — quarantined failures are ignored.\n"
        "  Learn more: https://docs.mergify.com/ci-insights/quarantine\n"
        "══════════════════════════════════════════\n"
        "❌ FAIL — No test cases found in the JUnit files\n"
        "  Check that your test step ran successfully before this step.\n"
        "  Exit code: 1\n"
        "══════════════════════════════════════════\n"
    )
