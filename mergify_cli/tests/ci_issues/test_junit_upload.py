import pathlib
from unittest import mock

from click import testing
import httpx
import pytest
import respx

from mergify_cli.ci import cli as cli_junit_upload
from mergify_cli.ci import junit_upload as junit_upload_mod


REPORT_XML = pathlib.Path(__file__).parent / "reports" / "report.xml"
PULL_REQUEST_EVENT = pathlib.Path(__file__).parent / "events" / "pull_request.json"


@pytest.mark.parametrize(
    ("env", "provider"),
    [
        (
            {
                "GITHUB_EVENT_NAME": "push",
                "GITHUB_ACTIONS": "true",
                "MERGIFY_API_URL": "https://api.mergify.com",
                "MERGIFY_TOKEN": "abc",
                "GITHUB_REPOSITORY": "user/repo",
                "GITHUB_SHA": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
                "GITHUB_WORKFLOW": "JOB",
            },
            "github_action",
        ),
        (
            {
                "GITHUB_ACTIONS": "",
                "CIRCLECI": "true",
                "MERGIFY_API_URL": "https://api.mergify.com",
                "MERGIFY_TOKEN": "abc",
                "CIRCLE_REPOSITORY_URL": "https://github.com/user/repo",
                "CIRCLE_SHA1": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
                "CIRCLE_JOB": "JOB",
            },
            "circleci",
        ),
    ],
)
def test_options_values_from_env_new(
    env: dict[str, str],
    provider: str,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    for key, value in env.items():
        monkeypatch.setenv(key, value)

    runner = testing.CliRunner()

    with mock.patch.object(
        junit_upload_mod,
        "upload",
        mock.AsyncMock(),
    ) as mocked_upload:
        result = runner.invoke(
            cli_junit_upload.junit_upload,
            [str(REPORT_XML)],
        )
    assert result.exit_code == 0
    assert mocked_upload.call_count == 1
    assert mocked_upload.call_args.kwargs == {
        "provider": provider,
        "api_url": "https://api.mergify.com",
        "token": "abc",
        "repository": "user/repo",
        "head_sha": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
        "job_name": "JOB",
        "files": (str(REPORT_XML),),
    }


def test_get_head_sha_github_actions(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("GITHUB_ACTIONS", "true")
    monkeypatch.setenv("GITHUB_EVENT_NAME", "pull_request")
    monkeypatch.setenv("GITHUB_EVENT_PATH", str(PULL_REQUEST_EVENT))

    assert (
        cli_junit_upload.get_github_actions_head_sha()
        == "ec26c3e57ca3a959ca5aad62de7213c562f8c821"
    )


@pytest.mark.parametrize(
    ("url", "api_url"),
    [
        ("https://enterprise-ghes.com", "https://enterprise-ghes.com/api/v3"),
        (
            "https://github.com",
            "https://api.github.com",
        ),
    ],
)
async def test_get_head_sha_circle_ci(
    url: str,
    api_url: str,
    monkeypatch: pytest.MonkeyPatch,
    respx_mock: respx.MockRouter,
) -> None:
    monkeypatch.setenv(
        "CIRCLE_PULL_REQUESTS",
        f"{url}/owner/repo/pulls/123",
    )
    respx_mock.get(
        f"{api_url}/repos/owner/repo/pulls/123",
    ).respond(
        200,
        json={"head": {"sha": "ec26c3e57ca3a959ca5aad62de7213c562f8c821"}},
    )

    assert (
        await cli_junit_upload.get_circle_ci_head_sha()
        == "ec26c3e57ca3a959ca5aad62de7213c562f8c821"
    )


def test_get_files_to_upload() -> None:
    with junit_upload_mod.get_files_to_upload(
        (str(REPORT_XML),),
    ) as files_to_upload:
        assert len(files_to_upload) == 1
        assert files_to_upload[0][1][0] == "report.xml"
        assert files_to_upload[0][1][1].read() == REPORT_XML.read_bytes()
        assert files_to_upload[0][1][2] == "application/xml"
        assert not files_to_upload[0][1][1].closed
    assert files_to_upload[0][1][1].closed


async def test_junit_upload(
    respx_mock: respx.MockRouter,
    capsys: pytest.CaptureFixture[str],
) -> None:
    gigid = "eyJjaV9qb2JfaWQiOjcwNzQyLCJzaWduYXR1cmUiOiI2NjcxN2QwZDdiZjZkMzAxMmFmNGE4NWQ1YTFlZDhmYjNkNDBjYmM4MmZjZjgxZTVmNzEzNzEyZjRlZjIxOTFmIn0="
    respx_mock.post(
        "/v1/repos/user/repo/ci_issues_upload",
    ).respond(
        200,
        json={"gigid": gigid},
    )

    await junit_upload_mod.upload(
        "https://api.mergify.com",
        "token",
        "user/repo",
        "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
        "ci-test-job",
        "circleci",
        (str(REPORT_XML),),
    )

    captured = capsys.readouterr()
    assert (
        captured.out.split("\n")[0]
        == f"::notice title=CI Issues report::CI_ISSUE_GIGID={gigid}"
    )


async def test_junit_upload_http_error(respx_mock: respx.MockRouter) -> None:
    respx_mock.post("/v1/repos/user/repo/ci_issues_upload").respond(
        422,
        json={"detail": "CI Issues is not enabled on this repository"},
    )

    with pytest.raises(httpx.HTTPStatusError):
        await junit_upload_mod.upload(
            "https://api.mergify.com",
            "token",
            "user/repo",
            "head-sha",
            "ci-job",
            "circleci",
            (str(REPORT_XML),),
        )
