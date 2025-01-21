import pathlib
import re

import pytest
import responses

from mergify_cli.ci import upload


REPORT_XML = pathlib.Path(__file__).parent / "report.xml"


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
    capsys: pytest.CaptureFixture[str],
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    for key, value in env.items():
        monkeypatch.setenv(key, value)

    responses.post(
        "https://api.mergify.com/v1/repos/user/repo/ci/traces",
    )

    await upload.upload(
        "https://api.mergify.com",
        "token",
        "user/repo",
        files=(str(REPORT_XML),),
    )

    captured = capsys.readouterr()
    if env["GITHUB_ACTIONS"] == "true":
        assert re.search(
            r"^::notice title=Mergify CI::MERGIFY_TRACE_ID=\d+",
            captured.out,
            re.MULTILINE,
        )
    else:
        assert "🎉 File(s) uploaded" in captured.out


@responses.activate(assert_all_requests_are_fired=True)
async def test_junit_upload_http_error() -> None:
    responses.post(
        "https://api.mergify.com/v1/repos/user/repo/ci/traces",
        status=422,
        json={"detail": "Not enabled on this repository"},
    )

    with pytest.raises(upload.UploadError):
        await upload.upload(
            "https://api.mergify.com",
            "token",
            "user/repo",
            (str(REPORT_XML),),
        )
