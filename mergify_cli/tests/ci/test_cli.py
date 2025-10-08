import json
import pathlib
from unittest import mock

import anys
import click
from click import testing
import pytest
import respx

from mergify_cli.ci import cli as ci_cli
from mergify_cli.ci.junit_processing import cli as junit_processing_cli
from mergify_cli.ci.junit_processing import quarantine
from mergify_cli.ci.junit_processing import upload


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
        mock.patch.object(
            quarantine,
            "check_and_update_failing_spans",
            return_value=0,
        ),
    ):
        result_process = runner.invoke(
            ci_cli.junit_process,
            [str(REPORT_XML)],
        )
        result_upload = runner.invoke(
            ci_cli.junit_upload,
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
        junit_processing_cli,
        "process_junit_files",
        mock.AsyncMock(),
    ) as mocked_process_junit_files:
        result = runner.invoke(
            ci_cli.junit_process,
            [str(REPORT_XML)],
        )

    assert result.exit_code == 0, result.stdout

    # Check that process_junit_files was called with the expected branch
    assert mocked_process_junit_files.call_count == 1
    call_kwargs = mocked_process_junit_files.call_args.kwargs
    assert call_kwargs["tests_target_branch"] == expected_branch


def test_quarantine_unhandled_error(monkeypatch: pytest.MonkeyPatch) -> None:
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
        ),
        mock.patch.object(
            quarantine,
            "check_and_update_failing_spans",
            side_effect=Exception("API crashed"),
        ) as mocked_quarantine,
    ):
        result = runner.invoke(
            ci_cli.junit_process,
            [str(REPORT_XML)],
        )

    assert result.exit_code == 1, (result.stdout, result.stderr)
    assert (
        result.stderr
        == """❌ An unexpected error occurred when checking quarantined tests: API crashed
This error occurred because there are failed tests in your CI pipeline and will disappear once your CI passes successfully.

If you're unsure why this is happening or need assistance, please contact Mergify to report the issue.
"""
    )
    assert (
        "FAIL — Unable to determine quarantined failures due to above error"
        in result.stdout
    )
    assert "MERGIFY_TEST_RUN_ID=" in result.stdout
    assert mocked_quarantine.call_count == 1


def test_quarantine_handled_error(monkeypatch: pytest.MonkeyPatch) -> None:
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
        ),
        mock.patch.object(
            quarantine,
            "check_and_update_failing_spans",
            side_effect=quarantine.QuarantineFailedError("It's not OK"),
        ) as mocked_quarantine,
    ):
        result = runner.invoke(
            ci_cli.junit_process,
            [str(REPORT_XML)],
        )
    assert result.exit_code == 1, (result.stdout, result.stderr)
    assert (
        result.stderr
        == """It's not OK
This error occurred because there are failed tests in your CI pipeline and will disappear once your CI passes successfully.

If you're unsure why this is happening or need assistance, please contact Mergify to report the issue.
"""
    )
    assert (
        "FAIL — Unable to determine quarantined failures due to above error"
        in result.stdout
    )
    assert "MERGIFY_TEST_RUN_ID=" in result.stdout
    assert mocked_quarantine.call_count == 1


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
        mock.patch.object(
            quarantine,
            "check_and_update_failing_spans",
            return_value=0,
        ),
    ):
        mocked_upload.side_effect = Exception("Upload failed")
        result = runner.invoke(
            ci_cli.junit_process,
            [str(REPORT_XML)],
        )
    assert result.exit_code == 0, (result.stdout, result.stderr)
    assert result.stderr == "❌ Error uploading JUnit XML reports: Upload failed\n"
    assert "MERGIFY_TEST_RUN_ID=" in result.stdout
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
        ci_cli._process_tests_target_branch(
            context_mock,
            param_mock,
            "refs/heads/main",
        )
        == "main"
    )
    assert (
        ci_cli._process_tests_target_branch(
            context_mock,
            param_mock,
            "refs/heads/feature-branch",
        )
        == "feature-branch"
    )

    # Test not stripping other prefixes
    assert (
        ci_cli._process_tests_target_branch(
            context_mock,
            param_mock,
            "refs/tags/v1.0.0",
        )
        == "refs/tags/v1.0.0"
    )
    assert (
        ci_cli._process_tests_target_branch(
            context_mock,
            param_mock,
            "main",
        )
        == "main"
    )

    # Test None value
    assert (
        ci_cli._process_tests_target_branch(
            context_mock,
            param_mock,
            None,
        )
        is None
    )

    # Test empty string
    assert not ci_cli._process_tests_target_branch(
        context_mock,
        param_mock,
        "",
    )


def test_junit_file_not_found_error_message() -> None:
    """Test that a helpful error message is shown when JUnit file doesn't exist."""
    runner = testing.CliRunner()

    # Set up minimal environment variables
    env = {
        "MERGIFY_API_URL": "https://api.mergify.com",
        "MERGIFY_TOKEN": "abc",
        "GITHUB_REPOSITORY": "user/repo",
        "GITHUB_BASE_REF": "main",
    }

    with runner.isolated_filesystem():
        # Try to run junit-process with a non-existent file
        result = runner.invoke(
            ci_cli.junit_process,
            ["non_existent_junit.xml"],
            env=env,
        )

        assert result.exit_code == 2  # Click parameter validation error
        assert "non_existent_junit.xml" in result.output
        assert "does not exist" in result.output
        assert "previous CI step failed" in result.output
        assert (
            "check if your test execution step completed successfully" in result.output
        )


@pytest.mark.respx(base_url="https://api.github.com/")
def test_scopes_send(
    respx_mock: respx.MockRouter,
    tmp_path: pathlib.Path,
) -> None:
    """Test scopes command with all required parameters."""

    # Create config file
    scopes_file = tmp_path / "scopes.json"
    scopes_file.write_text(
        json.dumps({"base_ref": "main", "scopes": ["backend", "frontend"]}),
    )

    runner = testing.CliRunner()

    post_mock = respx_mock.post(
        "https://api.mergify.com/v1/repos/owner/repository/pulls/123/scopes",
        headers={"Authorization": "Bearer test-token"},
    ).respond(200)
    result = runner.invoke(
        ci_cli.scopes_send,
        [
            "--pull-request",
            "123",
            "--repository",
            "owner/repository",
            "--token",
            "test-token",
            "--scope",
            "foobar",
            "--file",
            str(scopes_file),
        ],
    )

    assert result.exit_code == 0, result.output
    payload = json.loads(post_mock.calls[0].request.content)
    assert sorted(payload["scopes"]) == ["backend", "foobar", "frontend"]
