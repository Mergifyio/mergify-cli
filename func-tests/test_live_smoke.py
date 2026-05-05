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
"""Live smoke tests against the real Mergify API.

Run nightly via `func-tests-live.yaml` against
`mergify-clients-testing/mergify-cli-repo` PR #1. Catches the
class of mock drift that schema-level checks miss: real auth
behavior, real serialization, response shapes the mock pretends
about. Skipped unless `LIVE_TEST_MERGIFY_TOKEN` is set.
"""

from __future__ import annotations

import pathlib
import typing

import pytest


pytestmark = pytest.mark.live


API_URL = "https://api.mergify.com"
REPOSITORY = "mergify-clients-testing/mergify-cli-repo"
PULL_REQUEST = 1

JUNIT_PASS = pathlib.Path(__file__).parent / "fixtures" / "junit_pass.xml"


def test_scopes_send_against_real_api(
    live_token: str,
    cli: typing.Callable[..., typing.Any],
) -> None:
    result = cli(
        "ci",
        "scopes-send",
        "--api-url",
        API_URL,
        "--token",
        live_token,
        "--repository",
        REPOSITORY,
        "--pull-request",
        str(PULL_REQUEST),
        "--scope",
        "func-tests-live-smoke",
    )
    assert result.returncode == 0, f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"


def test_junit_process_against_real_api(
    live_token: str,
    cli: typing.Callable[..., typing.Any],
) -> None:
    result = cli(
        "ci",
        "junit-process",
        "--api-url",
        API_URL,
        "--token",
        live_token,
        "--repository",
        REPOSITORY,
        "--tests-target-branch",
        "main",
        str(JUNIT_PASS),
    )
    assert result.returncode == 0, f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
