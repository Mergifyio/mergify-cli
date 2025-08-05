import pathlib
from unittest import mock

import anys
import click
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
                "GITHUB_BASE_REF": "main",
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
                "GITHUB_REF_NAME": "main",
            },
            id="CircleCI",
        ),
    ],
)
def test_cli(env: dict[str, str], monkeypatch: pytest.MonkeyPatch) -> None:
    for key, value in env.items():
        monkeypatch.setenv(key, value)

    runner = testing.CliRunner()

    with (
        mock.patch.object(
            upload,
            "upload",
            mock.Mock(),
        ) as mocked_upload,
        mock.patch.object(cli_junit_upload, "check_failing_spans_with_quarantine"),
    ):
        result_process = runner.invoke(
            cli_junit_upload.junit_process,
            [str(REPORT_XML)],
        )
        result_upload = runner.invoke(
            cli_junit_upload.junit_upload,
            [str(REPORT_XML)],
        )
    assert result_process.exit_code == 0, result_process.stdout
    assert result_upload.exit_code == 0, result_upload.stdout
    assert mocked_upload.call_count == 2
    assert mocked_upload.call_args.kwargs == {
        "api_url": "https://api.mergify.com",
        "token": "abc",
        "repository": "user/repo",
        "spans": anys.ANY_LIST,
    }


@pytest.mark.parametrize(
    ("env", "expected_branch"),
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
                "GITHUB_REF": "refs/heads/feature-branch",
            },
            "feature-branch",
            id="GITHUB_REF with refs/heads/ prefix",
        ),
        pytest.param(
            {
                "GITHUB_EVENT_NAME": "push",
                "GITHUB_ACTIONS": "true",
                "MERGIFY_API_URL": "https://api.mergify.com",
                "MERGIFY_TOKEN": "abc",
                "GITHUB_REPOSITORY": "user/repo",
                "GITHUB_SHA": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
                "GITHUB_WORKFLOW": "JOB",
                "GITHUB_REF": "main",
            },
            "main",
            id="GITHUB_REF without refs/heads/ prefix",
        ),
        pytest.param(
            {
                "GITHUB_EVENT_NAME": "push",
                "GITHUB_ACTIONS": "true",
                "MERGIFY_API_URL": "https://api.mergify.com",
                "MERGIFY_TOKEN": "abc",
                "GITHUB_REPOSITORY": "user/repo",
                "GITHUB_SHA": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
                "GITHUB_WORKFLOW": "JOB",
                "GITHUB_BASE_REF": "main",
                "GITHUB_REF": "refs/heads/feature-branch",
            },
            "main",
            id="GITHUB_BASE_REF takes precedence over GITHUB_REF",
        ),
        pytest.param(
            {
                "GITHUB_EVENT_NAME": "push",
                "GITHUB_ACTIONS": "true",
                "MERGIFY_API_URL": "https://api.mergify.com",
                "MERGIFY_TOKEN": "abc",
                "GITHUB_REPOSITORY": "user/repo",
                "GITHUB_SHA": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
                "GITHUB_WORKFLOW": "JOB",
                "GITHUB_REF": "refs/tags/v1.0.0",
            },
            "refs/tags/v1.0.0",
            id="GITHUB_REF with refs/tags/ prefix (should not be stripped)",
        ),
    ],
)
def test_tests_target_branch_environment_variable_processing(
    env: dict[str, str],
    expected_branch: str,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Test that tests_target_branch environment variable processing works correctly."""
    for key in [
        "GITHUB_REF",
        "GITHUB_REF_NAME",
        "GITHUB_BASE_REF",
    ]:  # Override value from CI runner
        monkeypatch.delenv(key, raising=False)

    for key, value in env.items():
        monkeypatch.setenv(key, value)

    runner = testing.CliRunner()

    with mock.patch.object(
        cli_junit_upload,
        "_process_junit_files",
        mock.AsyncMock(),
    ) as mocked_process_junit_files:
        result = runner.invoke(
            cli_junit_upload.junit_process,
            [str(REPORT_XML)],
        )

    assert result.exit_code == 0, result.stdout

    # Check that _process_junit_files was called with the expected branch
    assert mocked_process_junit_files.call_count == 1
    call_kwargs = mocked_process_junit_files.call_args.kwargs
    assert call_kwargs["tests_target_branch"] == expected_branch


def test_upload_error(monkeypatch: pytest.MonkeyPatch) -> None:
    for key, value in {
        "GITHUB_EVENT_NAME": "push",
        "GITHUB_ACTIONS": "true",
        "MERGIFY_API_URL": "https://api.mergify.com",
        "MERGIFY_TOKEN": "abc",
        "GITHUB_REPOSITORY": "user/repo",
        "GITHUB_SHA": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
        "GITHUB_WORKFLOW": "JOB",
        "GITHUB_BASE_REF": "main",
    }.items():
        monkeypatch.setenv(key, value)

    runner = testing.CliRunner()

    with (
        mock.patch.object(
            upload,
            "upload",
            mock.Mock(),
        ) as mocked_upload,
        mock.patch.object(cli_junit_upload, "check_failing_spans_with_quarantine"),
    ):
        mocked_upload.side_effect = Exception("Upload failed")
        result = runner.invoke(
            cli_junit_upload.junit_process,
            [str(REPORT_XML)],
        )
    assert result.exit_code == 0, (result.stdout, result.stderr)
    assert result.stderr == "Error uploading JUnit XML reports: Upload failed\n"
    assert result.stdout.startswith("MERGIFY_TEST_RUN_ID=")
    assert mocked_upload.call_count == 1
    assert mocked_upload.call_args.kwargs == {
        "api_url": "https://api.mergify.com",
        "token": "abc",
        "repository": "user/repo",
        "spans": anys.ANY_LIST,
    }


def test_process_tests_target_branch_callback() -> None:
    """Test the _process_tests_target_branch callback function directly."""
    context_mock = mock.MagicMock(spec=click.Context)
    param_mock = mock.MagicMock(spec=click.Parameter)

    # Test stripping refs/heads/ prefix
    assert (
        cli_junit_upload._process_tests_target_branch(
            context_mock,
            param_mock,
            "refs/heads/main",
        )
        == "main"
    )
    assert (
        cli_junit_upload._process_tests_target_branch(
            context_mock,
            param_mock,
            "refs/heads/feature-branch",
        )
        == "feature-branch"
    )

    # Test not stripping other prefixes
    assert (
        cli_junit_upload._process_tests_target_branch(
            context_mock,
            param_mock,
            "refs/tags/v1.0.0",
        )
        == "refs/tags/v1.0.0"
    )
    assert (
        cli_junit_upload._process_tests_target_branch(
            context_mock,
            param_mock,
            "main",
        )
        == "main"
    )

    # Test None value
    assert (
        cli_junit_upload._process_tests_target_branch(
            context_mock,
            param_mock,
            None,
        )
        is None
    )

    # Test empty string
    assert not cli_junit_upload._process_tests_target_branch(
        context_mock,
        param_mock,
        "",
    )
