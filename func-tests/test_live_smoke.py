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


def test_queue_status(
    live_admin_token: str,
    cli: typing.Callable[..., typing.Any],
) -> None:
    """`GET /v1/repos/{owner}/{repo}/merge-queue/status`.

    Uses the admin-scoped token because all queue endpoints
    (read or write) require queue-management scope on the test
    repo; the CI-scoped token is rejected with 403.

    ``--json`` mode is a passthrough of the API response, so the
    smoke test only checks that the call succeeds and parses as
    JSON — the contract we want preserved across the Python →
    Rust port is the URL, the auth, and that the response is
    valid JSON.
    """
    import json

    # Group-level options (``--token`` / ``--api-url`` /
    # ``--repository``) come BEFORE the subcommand. Click requires
    # this for the Python implementation (the options live on the
    # ``@queue`` group); Rust accepts both orders via clap's
    # ``global = true``. Put them on the group so the same test
    # works against both ends of the port.
    result = cli(
        "queue",
        "--api-url",
        API_URL,
        "--token",
        live_admin_token,
        "--repository",
        REPOSITORY,
        "status",
        "--json",
    )
    assert result.returncode == 0, (
        f"queue status failed\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        pytest.fail(
            f"queue status --json emitted non-JSON output\n"
            f"error: {exc}\nstdout:\n{result.stdout}",
        )
    assert isinstance(payload, dict), (
        f"queue status --json must emit a JSON object\nstdout:\n{result.stdout}"
    )


def test_queue_show_not_in_queue(
    live_admin_token: str,
    cli: typing.Callable[..., typing.Any],
) -> None:
    """`GET /v1/repos/{owner}/{repo}/merge-queue/pull/{n}` 404 path.

    Uses the admin-scoped token because all queue endpoints (read
    or write) require queue-management scope on the test repo;
    the CI-scoped token is rejected with 403.

    Calls with a PR number that is almost certainly not in the
    queue (the test repo has far fewer than this many PRs).
    Both Python and Rust special-case 404 with the same
    user-facing message and ``MERGIFY_API_ERROR`` exit code (6)
    — that contract is what this test pins.

    Testing the 404 path (instead of a real queued PR) makes the
    test independent of whether PR #1 happens to be queued at run
    time. The endpoint reachability, auth, and 404 mapping are
    the parts that would silently break on a URL or schema drift.
    """
    # Group-level options come BEFORE the subcommand — same
    # invocation shape as the queue status smoke test (Click
    # requires it for Python, Rust accepts both via clap's
    # ``global = true``).
    result = cli(
        "queue",
        "--api-url",
        API_URL,
        "--token",
        live_admin_token,
        "--repository",
        REPOSITORY,
        "show",
        "99999999",
    )
    assert result.returncode == 6, (
        f"expected MERGIFY_API_ERROR (6), got {result.returncode}\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    combined = (result.stdout + result.stderr).lower()
    assert "not in the merge queue" in combined, (
        f"expected 'not in the merge queue' message\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )


def test_freeze_list(
    live_admin_token: str,
    cli: typing.Callable[..., typing.Any],
) -> None:
    """`GET /v1/repos/{owner}/{repo}/scheduled_freeze`.

    Uses the admin-scoped token because scheduled-freeze endpoints
    sit under the queue-management family; the CI-scoped token is
    rejected with 403.

    ``--json`` mode is a passthrough of the inner
    ``scheduled_freezes`` array (Python's ``list_freezes`` returns
    ``data["scheduled_freezes"]``, the CLI prints that verbatim).
    The smoke test only checks the call succeeds and parses as a
    JSON array — the contract preserved across the Python → Rust
    port is the URL, the auth, and the array shape of the
    ``--json`` output.
    """
    import json

    # Group-level options (``--token`` / ``--api-url`` /
    # ``--repository``) come BEFORE the subcommand — Click requires
    # it on the Python side (options live on ``@freeze``), Rust
    # accepts both via clap's ``global = true``.
    result = cli(
        "freeze",
        "--api-url",
        API_URL,
        "--token",
        live_admin_token,
        "--repository",
        REPOSITORY,
        "list",
        "--json",
    )
    assert result.returncode == 0, (
        f"freeze list failed\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        pytest.fail(
            f"freeze list --json emitted non-JSON output\n"
            f"error: {exc}\nstdout:\n{result.stdout}",
        )
    assert isinstance(payload, list), (
        f"freeze list --json must emit a JSON array\nstdout:\n{result.stdout}"
    )


def test_freeze_create_update_delete_roundtrip(
    live_admin_token: str,
    cli: typing.Callable[..., typing.Any],
) -> None:
    """`POST` + `PATCH` + `POST .../{id}/delete` round-trip on
    ``/v1/repos/{owner}/{repo}/scheduled_freeze``.

    Uses the admin-scoped token (scheduled-freeze endpoints sit
    under the queue-management family and the CI-scoped token is
    rejected with 403).

    The roundtrip schedules a freeze far in the future so we don't
    disturb real merges in the test repo. Cleanup runs from
    ``finally`` so the freeze is deleted even if an assertion in
    the middle of the test fails — and the test is also tolerant
    of a leaked freeze from a previous interrupted run (the create
    still succeeds because each run uses a unique reason; the
    orphan can be cleaned up out of band).

    The Mergify API requires ``delete_reason`` on every delete
    (the Python ``--reason`` help text says "required if freeze is
    active", but the server returns 422 for a missing key
    regardless of the freeze's active state). The test always
    passes ``--reason`` so the cleanup succeeds even on the
    server-validated path.
    """
    import re
    import time

    # Unique reason so concurrent or repeated runs don't fight over
    # the same row.
    reason = f"func-tests-live-smoke-{int(time.time())}"

    create = cli(
        "freeze",
        "--api-url",
        API_URL,
        "--token",
        live_admin_token,
        "--repository",
        REPOSITORY,
        "create",
        "--reason",
        reason,
        "--timezone",
        "UTC",
        "--start",
        "2099-01-01T00:00:00",
        "--end",
        "2099-01-02T00:00:00",
    )
    assert create.returncode == 0, (
        f"freeze create failed\nstdout:\n{create.stdout}\nstderr:\n{create.stderr}"
    )
    # The Python create command prints the freeze body via
    # ``_print_freeze`` — pull the UUID out so we can target the
    # update + delete by ID. The Rust port emits the same human
    # block, so this regex pins both ends of the port.
    match = re.search(r"ID:\s+([0-9a-fA-F-]{36})", create.stdout)
    assert match, f"could not find freeze ID in create output\nstdout:\n{create.stdout}"
    freeze_id = match.group(1)

    try:
        update = cli(
            "freeze",
            "--api-url",
            API_URL,
            "--token",
            live_admin_token,
            "--repository",
            REPOSITORY,
            "update",
            freeze_id,
            "--reason",
            f"{reason}-updated",
        )
        assert update.returncode == 0, (
            f"freeze update failed\nstdout:\n{update.stdout}\nstderr:\n{update.stderr}"
        )
        assert f"{reason}-updated" in update.stdout, (
            f"freeze update did not echo the new reason\n"
            f"stdout:\n{update.stdout}\nstderr:\n{update.stderr}"
        )
    finally:
        delete = cli(
            "freeze",
            "--api-url",
            API_URL,
            "--token",
            live_admin_token,
            "--repository",
            REPOSITORY,
            "delete",
            freeze_id,
            "--reason",
            f"{reason}-cleanup",
        )

    assert delete.returncode == 0, (
        f"freeze delete failed\nstdout:\n{delete.stdout}\nstderr:\n{delete.stderr}"
    )
    assert "deleted" in delete.stdout.lower(), (
        f"freeze delete did not print confirmation\n"
        f"stdout:\n{delete.stdout}\nstderr:\n{delete.stderr}"
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
