import pathlib
from unittest import mock

from opentelemetry.sdk.trace import id_generator
import pytest

from mergify_cli.ci.junit_processing import cli
from mergify_cli.ci.junit_processing import quarantine
from mergify_cli.ci.junit_processing import upload


REPORT_XML = pathlib.Path(__file__).parent.parent / "report.xml"


async def test_process_junit_file_reporting(
    capsys: pytest.CaptureFixture[str],
) -> None:
    with (
        mock.patch.object(
            quarantine,
            "check_and_update_failing_spans",
            return_value=0,
        ),
        mock.patch.object(upload, "upload"),
        mock.patch.object(
            id_generator.RandomIdGenerator,
            "generate_span_id",
            return_value=12345678910,
        ),
        pytest.raises(SystemExit),
    ):
        await cli.process_junit_files(
            api_url="https://api.mergify.com",
            token="foobar",  # noqa: S106
            repository="foo/bar",
            test_framework=None,
            test_language=None,
            tests_target_branch="main",
            files=(str(REPORT_XML),),
        )

    captured = capsys.readouterr()
    assert (
        captured.out
        == """🚀 CI Insights · Upload JUnit
────────────────────────────
📂 Discovered reports: 1
🛠️ MERGIFY_TEST_RUN_ID=00000002dfdc1c3e
🧪 Parsed tests: 2 (✅ passed: 1 | ❌ failed: 1)

🎉 Verdict
• Status: ✅ OK — all 1 failures are quarantined (ignored for CI status)
• Exit code: 0
"""
    )
