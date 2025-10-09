#
#  Copyright © 2021-2024 Mergify SAS
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


import collections
import json
import pathlib
from unittest import mock

import pytest

from mergify_cli import utils


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
    assert event == event_data


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
