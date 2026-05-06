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

Driven by `func-tests-live.yaml` on every PR against
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

JUNIT_FAIL = pathlib.Path(__file__).parent / "fixtures" / "junit_fail.xml"


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
    """OTLP traces upload + quarantine check round-trip.

    Uses a fixture with one failing test so the quarantine endpoint
    is actually called (`junit-process` short-circuits the
    quarantine call when the report has zero failures, which makes
    the all-passing fixture useless as a canary). Asserts on stdout
    rather than exit code, because:

    - `junit-process` swallows OTLP upload errors into a stdout
      warning ("reports not uploaded") without affecting the exit
      code, so a 5xx on `/ci/traces` would not surface as failure.
    - The exit code reflects whether failures are quarantined on
      the live tenant, which is a state the tests don't control.

    A green run is one where neither endpoint logged an error
    string into stdout.
    """
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
        str(JUNIT_FAIL),
    )

    assert " not uploaded" not in result.stdout, (
        f"OTLP traces endpoint did not accept upload\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    assert "Failed to check quarantine" not in result.stdout, (
        f"quarantine endpoint did not respond\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
