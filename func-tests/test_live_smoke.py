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
`mergify-clients-testing/mergify-cli-repo` PR #1. Each test fires
when the real API's URL, auth, or wire format diverges from what
the CLI expects. Skipped unless `LIVE_TEST_MERGIFY_TOKEN` is set.
"""

from __future__ import annotations

import pathlib
import typing

import pytest


pytestmark = pytest.mark.live


API_URL = "https://api.mergify.com"
REPOSITORY = "mergify-clients-testing/mergify-cli-repo"
PULL_REQUEST = 1
PULL_REQUEST_URL = f"https://github.com/{REPOSITORY}/pull/{PULL_REQUEST}"

JUNIT_PASS = pathlib.Path(__file__).parent / "fixtures" / "junit_pass.xml"


def test_scopes_send(
    live_token: str,
    cli: typing.Callable[..., typing.Any],
) -> None:
    """`POST /v1/repos/{owner}/{repo}/pulls/{n}/scopes`."""
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


def test_junit_process(
    live_token: str,
    cli: typing.Callable[..., typing.Any],
) -> None:
    """OTLP traces upload + quarantine check round-trip."""
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


def test_config_simulate(
    live_token: str,
    cli: typing.Callable[..., typing.Any],
    tmp_path: pathlib.Path,
) -> None:
    """`POST /v1/repos/{owner}/{repo}/pulls/{n}/simulator`."""
    config = tmp_path / ".mergify.yml"
    config.write_text("pull_request_rules: []\n")
    result = cli(
        "config",
        "--config-file",
        str(config),
        "simulate",
        "--api-url",
        API_URL,
        "--token",
        live_token,
        PULL_REQUEST_URL,
    )
    assert result.returncode == 0, f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
