import pathlib
from unittest import mock

from click import testing
import pytest

from mergify_cli.ci import cli as cli_junit_upload
from mergify_cli.ci import upload


REPORT_XML = pathlib.Path(__file__).parent / "report.xml"


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
def test_cli(env: dict[str, str], monkeypatch: pytest.MonkeyPatch) -> None:
    for key, value in env.items():
        monkeypatch.setenv(key, value)

    runner = testing.CliRunner()

    with mock.patch.object(
        upload,
        "upload",
        mock.AsyncMock(),
    ) as mocked_upload:
        result = runner.invoke(
            cli_junit_upload.junit_upload,
            [str(REPORT_XML)],
        )
    assert result.exit_code == 0, result.stdout
    assert mocked_upload.call_count == 1
    assert mocked_upload.call_args.kwargs == {
        "api_url": "https://api.mergify.com",
        "token": "abc",
        "repository": "user/repo",
        "test_framework": None,
        "test_language": None,
        "files": (str(REPORT_XML),),
    }


def test_upload_error(monkeypatch: pytest.MonkeyPatch) -> None:
    for key, value in {
        "GITHUB_EVENT_NAME": "push",
        "GITHUB_ACTIONS": "true",
        "MERGIFY_API_URL": "https://api.mergify.com",
        "MERGIFY_TOKEN": "abc",
        "GITHUB_REPOSITORY": "user/repo",
        "GITHUB_SHA": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
        "GITHUB_WORKFLOW": "JOB",
    }.items():
        monkeypatch.setenv(key, value)

    runner = testing.CliRunner()

    with mock.patch.object(
        upload,
        "upload",
        mock.AsyncMock(),
    ) as mocked_upload:
        mocked_upload.side_effect = Exception("Upload failed")
        result = runner.invoke(
            cli_junit_upload.junit_upload,
            [str(REPORT_XML)],
        )
    assert result.exit_code == 0, (result.stdout, result.stderr)
    assert result.stderr == "Error uploading JUnit XML reports: Upload failed\n"
    assert not result.stdout
    assert mocked_upload.call_count == 1
    assert mocked_upload.call_args.kwargs == {
        "api_url": "https://api.mergify.com",
        "token": "abc",
        "repository": "user/repo",
        "test_framework": None,
        "test_language": None,
        "files": (str(REPORT_XML),),
    }
