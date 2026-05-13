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
the CLI expects. API-hitting tests skip unless their token
(`LIVE_TEST_MERGIFY_TOKEN_CI` or `_ADMIN`) is set; locally-evaluated
tests run unconditionally.
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


def test_queue_pause_unpause_roundtrip(
    live_admin_token: str,
    cli: typing.Callable[..., typing.Any],
) -> None:
    """`PUT` + `DELETE /v1/repos/{owner}/{repo}/merge-queue/pause`.

    Uses the admin-scoped token because pause/unpause hits the
    queue-admin endpoint and the CI-scoped token is rejected
    (403) by design.

    Runs the pause and unpause commands as a single round-trip so
    the test repo's queue is left in the same state we found it
    in, even when an assertion fails (the unpause runs from
    ``finally``). This means the test is also tolerant of a leaked
    paused state from a previous interrupted run — the second pause
    just refreshes the reason.
    """
    pause = cli(
        "queue",
        "pause",
        "--api-url",
        API_URL,
        "--token",
        live_admin_token,
        "--repository",
        REPOSITORY,
        "--reason",
        "func-tests-live-smoke",
        "--yes-i-am-sure",
    )
    try:
        assert pause.returncode == 0, (
            f"queue pause failed\nstdout:\n{pause.stdout}\nstderr:\n{pause.stderr}"
        )
        assert "Queue paused" in pause.stdout, (
            f"queue pause did not print confirmation\n"
            f"stdout:\n{pause.stdout}\nstderr:\n{pause.stderr}"
        )
    finally:
        unpause = cli(
            "queue",
            "unpause",
            "--api-url",
            API_URL,
            "--token",
            live_admin_token,
            "--repository",
            REPOSITORY,
        )

    assert unpause.returncode == 0, (
        f"queue unpause failed\nstdout:\n{unpause.stdout}\nstderr:\n{unpause.stderr}"
    )
    assert "Queue resumed" in unpause.stdout, (
        f"queue unpause did not print confirmation\n"
        f"stdout:\n{unpause.stdout}\nstderr:\n{unpause.stderr}"
    )


def test_ci_git_refs_fallback(
    cli: typing.Callable[..., typing.Any],
) -> None:
    """`mergify ci git-refs` falls back to ``HEAD^..HEAD`` when no
    CI provider env is set.

    Doesn't need ``live_token`` — the command is locally evaluated
    (no API call). The conftest fixture scrubs every CI/event env
    var and runs in a tmp dir, so the detector lands on its
    literal-string fallback path. This is the same smoke test we
    want to keep working when the command moves from Python to
    Rust — same contract, both ends of the port.
    """
    result = cli("ci", "git-refs")
    assert result.returncode == 0, f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    # Pin the exact two-line output. Substring matches would let
    # added lines or rearrangements slip through silently, which
    # defeats the "pin the contract" intent. The Python and Rust
    # implementations both emit precisely this text on the
    # fallback path.
    assert result.stdout == "Base: HEAD^\nHead: HEAD\n", (
        f"output drifted from the pinned format\nstdout:\n{result.stdout!r}"
    )


def test_ci_queue_info_outside_mq(
    cli: typing.Callable[..., typing.Any],
) -> None:
    """`mergify ci queue-info` exits ``INVALID_STATE`` (7) when not
    running on an MQ draft PR.

    Doesn't need ``live_token`` — the command is locally
    evaluated. The conftest fixture scrubs every event env var
    and runs in a tmp dir, so the detector always reports
    "no MQ context". This is the contract we want preserved
    across the upcoming Python → Rust port.
    """
    result = cli("ci", "queue-info")
    assert result.returncode == 7, (
        f"expected INVALID_STATE (7), got {result.returncode}\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    combined = (result.stdout + result.stderr).lower()
    assert "merge queue" in combined, (
        f"expected MQ-context message\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )


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
