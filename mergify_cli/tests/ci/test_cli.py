from __future__ import annotations

import pathlib
from unittest import mock

import anys
import click
from click import testing
import pytest

from mergify_cli.ci import cli as ci_cli
from mergify_cli.ci.junit_processing import cli as junit_processing_cli
from mergify_cli.ci.junit_processing import quarantine
from mergify_cli.ci.junit_processing import upload
from mergify_cli.exit_codes import ExitCode


FIXTURES_DIR = pathlib.Path(__file__).parent / "fixtures"
REPORT_XML = FIXTURES_DIR / "report.xml"


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
            return_value=quarantine.QuarantineResult(
                failing_spans=[],
                quarantined_spans=[],
                non_quarantined_spans=[],
                failing_tests_not_quarantined_count=0,
            ),
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
                "GITHUB_BASE_REF": "main",
                "GITHUB_HEAD_REF": "feature-branch",
            },
            "main",
            id="GITHUB_BASE_REF takes precedence over GITHUB_HEAD_REF",
        ),
        pytest.param(
            {
                "GITHUB_EVENT_NAME": "pull_request",
                "GITHUB_ACTIONS": "true",
                "MERGIFY_API_URL": "https://api.mergify.com",
                "MERGIFY_TOKEN": "abc",
                "GITHUB_REPOSITORY": "user/repo",
                "GITHUB_SHA": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
                "GITHUB_WORKFLOW": "JOB",
                "GITHUB_HEAD_REF": "feature-branch",
                "GITHUB_REF_NAME": "123/merge",
            },
            "feature-branch",
            id="GITHUB_HEAD_REF takes precedence over GITHUB_REF_NAME",
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
                "GITHUB_REF_NAME": "main",
            },
            "main",
            id="GITHUB_REF_NAME fallback when GITHUB_HEAD_REF is not set",
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
        "GITHUB_HEAD_REF",
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


@pytest.mark.parametrize(
    ("env", "expected_branch"),
    [
        pytest.param(
            {
                "BUILDKITE": "true",
                "MERGIFY_API_URL": "https://api.mergify.com",
                "MERGIFY_TOKEN": "abc",
                "BUILDKITE_REPO": "git@github.com:user/repo.git",
                "BUILDKITE_PULL_REQUEST_BASE_BRANCH": "main",
                "BUILDKITE_BRANCH": "feature-branch",
            },
            "main",
            id="BUILDKITE_PULL_REQUEST_BASE_BRANCH takes precedence",
        ),
        pytest.param(
            {
                "BUILDKITE": "true",
                "MERGIFY_API_URL": "https://api.mergify.com",
                "MERGIFY_TOKEN": "abc",
                "BUILDKITE_REPO": "git@github.com:user/repo.git",
                "BUILDKITE_BRANCH": "feature-branch",
            },
            "feature-branch",
            id="BUILDKITE_BRANCH fallback when no PR base branch",
        ),
    ],
)
def test_tests_target_branch_buildkite_environment_variable_processing(
    env: dict[str, str],
    expected_branch: str,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Test that --tests-target-branch is auto-detected from Buildkite env vars."""
    for key in [
        "GITHUB_ACTIONS",
        "GITHUB_REF",
        "GITHUB_REF_NAME",
        "GITHUB_HEAD_REF",
        "GITHUB_BASE_REF",
        "JENKINS_URL",
        "CIRCLECI",
        "BUILDKITE",
        "BUILDKITE_PULL_REQUEST_BASE_BRANCH",
        "BUILDKITE_BRANCH",
    ]:
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

    assert mocked_process_junit_files.call_count == 1
    call_kwargs = mocked_process_junit_files.call_args.kwargs
    assert call_kwargs["tests_target_branch"] == expected_branch


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


def test_expand_junit_patterns_literal_path() -> None:
    result = ci_cli._expand_junit_patterns(
        mock.Mock(),
        mock.Mock(),
        (str(REPORT_XML),),
    )
    assert result == (str(REPORT_XML),)


def test_expand_junit_patterns_glob_matches_multiple(
    tmp_path: pathlib.Path,
) -> None:
    first = tmp_path / "report_a.xml"
    second = tmp_path / "report_b.xml"
    first.write_text("")
    second.write_text("")

    result = ci_cli._expand_junit_patterns(
        mock.Mock(),
        mock.Mock(),
        (str(tmp_path / "report_*.xml"),),
    )

    assert set(result) == {str(first), str(second)}


def test_expand_junit_patterns_recursive_glob(tmp_path: pathlib.Path) -> None:
    top = tmp_path / "top.xml"
    nested = tmp_path / "nested" / "deep" / "inner.xml"
    nested.parent.mkdir(parents=True)
    top.write_text("")
    nested.write_text("")

    result = ci_cli._expand_junit_patterns(
        mock.Mock(),
        mock.Mock(),
        (str(tmp_path / "**" / "*.xml"),),
    )

    assert set(result) == {str(top), str(nested)}


def test_expand_junit_patterns_literal_takes_precedence_over_magic(
    tmp_path: pathlib.Path,
) -> None:
    literal = tmp_path / "report[1].xml"
    literal.write_text("")

    result = ci_cli._expand_junit_patterns(
        mock.Mock(),
        mock.Mock(),
        (str(literal),),
    )

    assert result == (str(literal),)


def test_expand_junit_patterns_directory_error(tmp_path: pathlib.Path) -> None:
    directory = tmp_path / "reports"
    directory.mkdir()

    with pytest.raises(click.BadParameter) as exc_info:
        ci_cli._expand_junit_patterns(
            mock.Mock(),
            mock.Mock(),
            (str(directory),),
        )

    assert "is a directory" in exc_info.value.message
    assert str(directory) in exc_info.value.message


def test_expand_junit_patterns_skips_directories(tmp_path: pathlib.Path) -> None:
    (tmp_path / "subdir").mkdir()
    only_file = tmp_path / "only.xml"
    only_file.write_text("")

    result = ci_cli._expand_junit_patterns(
        mock.Mock(),
        mock.Mock(),
        (str(tmp_path / "*"),),
    )

    assert result == (str(only_file),)


def test_expand_junit_patterns_zero_match_error(tmp_path: pathlib.Path) -> None:
    with pytest.raises(click.BadParameter) as exc_info:
        ci_cli._expand_junit_patterns(
            mock.Mock(),
            mock.Mock(),
            (str(tmp_path / "nonexistent-*.xml"),),
        )

    assert "did not match any file" in exc_info.value.message
    assert "nonexistent-*.xml" in exc_info.value.message


def test_expand_junit_patterns_dedupes_literal_and_glob(
    tmp_path: pathlib.Path,
) -> None:
    report = tmp_path / "report.xml"
    report.write_text("")

    result = ci_cli._expand_junit_patterns(
        mock.Mock(),
        mock.Mock(),
        (str(report), str(tmp_path / "*.xml")),
    )

    assert result == (str(report),)


def test_junit_process_glob_end_to_end(tmp_path: pathlib.Path) -> None:
    """Confirm the callback is wired on junit-process and expansion reaches the runner."""
    first = tmp_path / "report_one.xml"
    second = tmp_path / "report_two.xml"
    first.write_bytes(REPORT_XML.read_bytes())
    second.write_bytes(REPORT_XML.read_bytes())

    env = {
        "MERGIFY_API_URL": "https://api.mergify.com",
        "MERGIFY_TOKEN": "abc",
        "GITHUB_REPOSITORY": "user/repo",
        "GITHUB_BASE_REF": "main",
    }

    runner = testing.CliRunner()
    mocked_process = mock.AsyncMock()
    with mock.patch.object(
        junit_processing_cli,
        "process_junit_files",
        mocked_process,
    ):
        result = runner.invoke(
            ci_cli.junit_process,
            [str(tmp_path / "report_*.xml")],
            env=env,
        )

    assert result.exit_code == 0, result.output
    assert mocked_process.await_count == 1
    await_args = mocked_process.await_args
    assert await_args is not None
    assert set(await_args.kwargs["files"]) == {str(first), str(second)}


def test_junit_process_glob_no_match_error(tmp_path: pathlib.Path) -> None:
    env = {
        "MERGIFY_API_URL": "https://api.mergify.com",
        "MERGIFY_TOKEN": "abc",
        "GITHUB_REPOSITORY": "user/repo",
        "GITHUB_BASE_REF": "main",
    }

    runner = testing.CliRunner()
    result = runner.invoke(
        ci_cli.junit_process,
        [str(tmp_path / "missing-*.xml")],
        env=env,
    )

    assert result.exit_code == 2
    assert "did not match any file" in result.output
    assert "missing-*.xml" in result.output


def test_scopes_empty_mergify_config_env_uses_autodetection(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """When MERGIFY_CONFIG_PATH is set but empty, the config should be auto-detected."""
    config_file = tmp_path / ".mergify.yml"
    config_file.write_text("scopes:\n  source:\n    manual:\n")

    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("MERGIFY_CONFIG_PATH", "")

    runner = testing.CliRunner()
    result = runner.invoke(ci_cli.scopes, ["--base", "old", "--head", "new"])

    # The command found the auto-detected config and ran; source is manual so
    # ScopesError is raised -> CONFIGURATION_ERROR exit code.
    assert result.exit_code == ExitCode.CONFIGURATION_ERROR
    assert "source `manual` has been set" in result.output
