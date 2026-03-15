from __future__ import annotations

import pathlib

from opentelemetry.sdk.trace import ReadableSpan
import opentelemetry.trace.span
import pytest
import responses

from mergify_cli.ci.junit_processing import junit
from mergify_cli.ci.junit_processing import upload


FIXTURES_DIR = pathlib.Path(__file__).parent.parent / "fixtures"
REPORT_XML = FIXTURES_DIR / "report.xml"


@responses.activate(assert_all_requests_are_fired=True)
@pytest.mark.parametrize(
    "env",
    [
        pytest.param(
            {
                "GITHUB_EVENT_NAME": "push",
                "GITHUB_ACTIONS": "true",
                "MERGIFY_API_URL": "https://api.mergify.com",
                "MERGIFY_TOKEN": "abc",
                "GITHUB_REPOSITORY": "user/repo",
                "GITHUB_SHA": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
                "GITHUB_WORKFLOW": "JOB",
            },
            id="GitHub",
        ),
        pytest.param(
            {
                "GITHUB_ACTIONS": "",
                "CIRCLECI": "true",
                "MERGIFY_API_URL": "https://api.mergify.com",
                "MERGIFY_TOKEN": "abc",
                "CIRCLE_REPOSITORY_URL": "https://github.com/user/repo",
                "CIRCLE_SHA1": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
                "CIRCLE_JOB": "JOB",
            },
            id="CircleCI",
        ),
    ],
)
async def test_junit_upload(
    env: dict[str, str],
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    run_id, spans = await junit.files_to_spans(files=(str(REPORT_XML),))
    for key, value in env.items():
        monkeypatch.setenv(key, value)

    responses.post(
        "https://api.mergify.com/v1/repos/user/repo/ci/traces",
    )

    upload.upload(
        "https://api.mergify.com",
        "token",
        "user/repo",
        spans,
    )

    assert len(bytes.fromhex(run_id)) == 8


@responses.activate(assert_all_requests_are_fired=True)
def test_junit_upload_http_error() -> None:
    responses.post(
        "https://api.mergify.com/v1/repos/user/repo/ci/traces",
        status=422,
        json={"detail": "Not enabled on this repository"},
    )

    with pytest.raises(upload.UploadError):
        upload.upload_spans(
            "https://api.mergify.com",
            "token",
            "user/repo",
            [
                ReadableSpan(
                    name="hello",
                    context=opentelemetry.trace.span.SpanContext(
                        trace_id=1234,
                        span_id=324,
                        is_remote=False,
                    ),
                ),
            ],
        )


@responses.activate(assert_all_requests_are_fired=True)
async def test_junit_upload_http_error_raises() -> None:
    responses.post(
        "https://api.mergify.com/v1/repos/user/repo/ci/traces",
        status=422,
        json={"detail": "Not enabled on this repository"},
    )

    _run_id, spans = await junit.files_to_spans(files=(str(REPORT_XML),))
    with pytest.raises(upload.UploadError, match="422"):
        upload.upload(
            "https://api.mergify.com",
            "token",
            "user/repo",
            spans,
        )
