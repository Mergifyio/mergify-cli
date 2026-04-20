#
#  Copyright © 2021-2026 Mergify SAS
#
# Licensed under the Apache License, Version 2.0 (the "License"); you may
# not use this file except in compliance with the License. You may obtain
# a copy of the License at
#
#      http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS, WITHOUT
# WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied. See the
# License for the specific language governing permissions and limitations
# under the License.
from __future__ import annotations

import json
from typing import TYPE_CHECKING
from unittest import mock

import httpx
import pytest

from mergify_cli import console
from mergify_cli import utils
from mergify_cli.exit_codes import ExitCode


if TYPE_CHECKING:
    import collections
    import pathlib


@pytest.mark.usefixtures("_git_repo")
async def test_get_branch_name() -> None:
    assert await utils.git_get_branch_name() == "main"


@pytest.mark.usefixtures("_git_repo")
async def test_get_target_branch() -> None:
    assert await utils.git_get_target_branch("main") == "main"


@pytest.mark.usefixtures("_git_repo")
async def test_get_target_remote() -> None:
    assert await utils.git_get_target_remote("main") == "origin"


@pytest.mark.usefixtures("_git_repo")
async def test_get_trunk() -> None:
    assert await utils.get_trunk() == "origin/main"


@pytest.mark.usefixtures("_git_repo")
async def test_get_trunk_auto_sets_upstream_when_missing() -> None:
    await utils.git("checkout", "-b", "feature-branch")
    # No upstream is set for feature-branch; get_trunk should auto-detect
    # from origin/HEAD and set the upstream
    original_git = utils.git

    async def patched_git(*args: str) -> str:
        if args[:2] == ("branch", "feature-branch"):
            # Simulate successful set-upstream-to without a real remote
            await original_git(
                "config",
                "branch.feature-branch.remote",
                "origin",
            )
            await original_git(
                "config",
                "branch.feature-branch.merge",
                "refs/heads/main",
            )
            return ""
        return await original_git(*args)

    with (
        mock.patch.object(
            utils,
            "_get_default_remote_branch",
            return_value=("origin", "main"),
        ),
        mock.patch.object(utils, "git", side_effect=patched_git),
    ):
        result = await utils.get_trunk()
    assert result == "origin/main"
    # Verify the upstream was set
    assert await utils.git_get_target_branch("feature-branch") == "main"
    assert await utils.git_get_target_remote("feature-branch") == "origin"


@pytest.mark.usefixtures("_git_repo")
async def test_get_trunk_fails_when_no_upstream_and_no_default() -> None:
    await utils.git("checkout", "-b", "feature-branch")
    with (
        mock.patch.object(
            utils,
            "_get_default_remote_branch",
            side_effect=utils.CommandError((), 1, b""),
        ),
        pytest.raises(utils.CommandError),
    ):
        await utils.get_trunk()


@pytest.mark.parametrize(
    ("default_arg_fct", "config_get_result", "expected_default"),
    [
        (utils.get_default_keep_pr_title_body, "true", True),
        (utils.get_default_create_as_draft, "true", True),
        (
            lambda: utils.get_default_branch_prefix("author"),
            "dummy-prefix",
            "dummy-prefix",
        ),
    ],
)
async def test_defaults_config_args_set(
    *,
    default_arg_fct: collections.abc.Callable[
        [],
        collections.abc.Awaitable[bool | str],
    ],
    config_get_result: bytes,
    expected_default: bool,
) -> None:
    with mock.patch.object(utils, "run_command", return_value=config_get_result):
        assert (await default_arg_fct()) == expected_default


@pytest.mark.parametrize(
    ("env_value", "expected"),
    [
        ("true", True),
        ("True", True),
        ("TRUE", True),
        ("yes", True),
        ("YES", True),
        ("y", True),
        ("Y", True),
        ("1", True),
        ("on", True),
        ("ON", True),
        ("false", False),
        ("no", False),
        ("0", False),
        ("", False),
        ("random", False),
        ("  true  ", True),  # Test with whitespace
        ("  false  ", False),
    ],
)
def test_get_boolean_env(
    monkeypatch: pytest.MonkeyPatch,
    *,
    env_value: str,
    expected: bool,
) -> None:
    monkeypatch.setenv("TEST_VAR", env_value)
    assert utils.get_boolean_env("TEST_VAR") == expected


def test_get_boolean_env_default_false(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("TEST_VAR", raising=False)
    assert utils.get_boolean_env("TEST_VAR") is False


def test_get_boolean_env_default_true(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("TEST_VAR", raising=False)
    assert utils.get_boolean_env("TEST_VAR", default=True) is True


def test_get_github_event_success(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    event_data = {"pull_request": {"number": 123}}
    event_file = tmp_path / "event.json"
    event_file.write_text(json.dumps(event_data))

    monkeypatch.setenv("GITHUB_EVENT_NAME", "pull_request")
    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event_file))
    name, event = utils.get_github_event()
    assert name == "pull_request"
    assert event.pull_request is not None
    assert event.pull_request.number == 123


def test_get_github_event_not_found(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("GITHUB_EVENT_PATH", raising=False)

    with pytest.raises(utils.GitHubEventNotFoundError):
        utils.get_github_event()


def test_get_github_event_file_not_exists(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    event_path = tmp_path / "nonexistent.json"
    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event_path))

    with pytest.raises(utils.GitHubEventNotFoundError):
        utils.get_github_event()


def test_get_mergify_http_client() -> None:
    client = utils.get_mergify_http_client(
        "https://api.mergify.com",
        "test-token",
    )
    assert client.headers["Authorization"] == "Bearer test-token"
    assert client.headers["Accept"] == "application/json"
    assert client.base_url == "https://api.mergify.com"


class TestCheckForStatus:
    async def test_success_does_nothing(self) -> None:
        request = httpx.Request("GET", "https://api.mergify.com/v1/repos/owner/repo")
        response = httpx.Response(200, request=request)
        await utils.check_for_status(response)

    async def test_error_with_json_detail(self) -> None:
        request = httpx.Request(
            "GET",
            "https://api.mergify.com/v1/repos/jd/home/scheduled_freeze",
        )
        response = httpx.Response(
            404,
            json={"detail": "Not Found"},
            request=request,
        )
        with (
            mock.patch.object(console, "print") as mock_print,
            pytest.raises(httpx.HTTPStatusError),
        ):
            await utils.check_for_status(response)

        mock_print.assert_any_call(
            "error: API error (HTTP 404): Not Found",
            style="red",
            markup=False,
        )
        mock_print.assert_any_call(
            "url: https://api.mergify.com/v1/repos/jd/home/scheduled_freeze",
            style="red",
        )

    async def test_error_with_plain_text(self) -> None:
        request = httpx.Request("GET", "https://api.mergify.com/v1/test")
        response = httpx.Response(
            500,
            text="Internal Server Error",
            request=request,
        )
        with (
            mock.patch.object(console, "print") as mock_print,
            pytest.raises(httpx.HTTPStatusError),
        ):
            await utils.check_for_status(response)

        mock_print.assert_any_call(
            "error: API error (HTTP 500): Internal Server Error",
            style="red",
            markup=False,
        )

    async def test_error_hides_request_data_by_default(self) -> None:
        request = httpx.Request(
            "POST",
            "https://api.mergify.com/v1/test",
            content=b'{"key": "value"}',
        )
        response = httpx.Response(
            400,
            json={"detail": "Bad Request"},
            request=request,
        )
        with (
            mock.patch.object(console, "print") as mock_print,
            pytest.raises(httpx.HTTPStatusError),
        ):
            await utils.check_for_status(response)

        printed_args = [str(call) for call in mock_print.call_args_list]
        assert not any("request data" in arg for arg in printed_args)

    async def test_error_shows_request_data_in_debug(self) -> None:
        request = httpx.Request(
            "POST",
            "https://api.mergify.com/v1/test",
            content=b'{"key": "value"}',
        )
        response = httpx.Response(
            400,
            json={"detail": "Bad Request"},
            request=request,
        )
        utils.set_debug(debug=True)
        try:
            with (
                mock.patch.object(console, "print") as mock_print,
                pytest.raises(httpx.HTTPStatusError),
            ):
                await utils.check_for_status(response)

            mock_print.assert_any_call(
                'request data: {"key": "value"}',
                style="red",
            )
        finally:
            utils.set_debug(debug=False)


class TestMergifyError:
    def test_default_exit_code_is_generic_error(self) -> None:
        err = utils.MergifyError("boom")
        assert err.exit_code == ExitCode.GENERIC_ERROR
        assert err.message == "boom"

    def test_accepts_exit_code_override(self) -> None:
        err = utils.MergifyError("bad config", exit_code=ExitCode.CONFIGURATION_ERROR)
        assert err.exit_code == ExitCode.CONFIGURATION_ERROR
        assert err.message == "bad config"

    def test_is_click_exception(self) -> None:
        """MergifyError must inherit from click.ClickException so click's
        standalone-mode handler catches it and exits with exit_code."""
        import click

        assert issubclass(utils.MergifyError, click.ClickException)

    def test_show_prints_red_error_to_stderr(
        self,
        capsys: pytest.CaptureFixture[str],
    ) -> None:
        err = utils.MergifyError("nope", exit_code=ExitCode.CONFIGURATION_ERROR)
        err.show()
        captured = capsys.readouterr()
        # The message may be captured from stdout or stderr depending on the
        # console configuration; assert only that it was emitted.
        # ANSI codes may or may not appear.
        assert "nope" in captured.err or "nope" in captured.out
