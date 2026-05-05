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
"""Functional tests for `mergify ci junit-process` (and the
deprecated `junit-upload` alias).

These commands hit two Mergify endpoints:

- ``POST /v1/ci/{owner}/repositories/{repo}/quarantines/check`` —
  asks whether failing tests are quarantined.
- ``POST /v1/repos/{owner}/{repo}/ci/traces`` — OTLP traces upload
  for the JUnit results.

We assert the quarantine endpoint precisely (URL, headers, JSON
body). The OTLP traces upload is asserted only by method + auth
header + non-empty body — its payload is gzip-compressed protobuf,
which is painful to introspect from a black-box test and not the
contract we care about here.
"""

from __future__ import annotations

import pathlib
import typing


if typing.TYPE_CHECKING:
    from pytest_httpserver import HTTPServer


FIXTURES_DIR = pathlib.Path(__file__).parent / "fixtures"
JUNIT_PASS = FIXTURES_DIR / "junit_pass.xml"
JUNIT_FAIL = FIXTURES_DIR / "junit_fail.xml"


def _expect_quarantine_check(
    httpserver: HTTPServer,
    *,
    response: dict[str, list[str]],
    status: int = 200,
) -> None:
    httpserver.expect_request(
        "/v1/ci/owner/repositories/repo/quarantines/check",
        method="POST",
        headers={"Authorization": "Bearer test-token"},
    ).respond_with_json(response, status=status)


def _expect_traces_upload(httpserver: HTTPServer) -> None:
    httpserver.expect_request(
        "/v1/repos/owner/repo/ci/traces",
        method="POST",
        headers={"Authorization": "Bearer test-token"},
    ).respond_with_data("", status=200)


def test_junit_process_all_passing(
    httpserver: HTTPServer,
    cli: typing.Callable[..., typing.Any],
) -> None:
    """No failures → quarantine still queried (with empty list), traces uploaded, exit 0."""
    _expect_quarantine_check(
        httpserver,
        response={"quarantined_tests_names": [], "non_quarantined_tests_names": []},
    )
    _expect_traces_upload(httpserver)

    result = cli(
        "ci",
        "junit-process",
        "--api-url",
        httpserver.url_for("").rstrip("/"),
        "--token",
        "test-token",
        "--repository",
        "owner/repo",
        "--tests-target-branch",
        "main",
        str(JUNIT_PASS),
    )

    assert result.returncode == 0, f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    assert "OK" in result.stdout
    # Traces endpoint must have been hit.
    assert any(
        req.path == "/v1/repos/owner/repo/ci/traces" for req, _ in httpserver.log
    ), "OTLP traces endpoint was not called"


def test_junit_process_failure_quarantined(
    httpserver: HTTPServer,
    cli: typing.Callable[..., typing.Any],
) -> None:
    """Failing test that *is* quarantined → exit 0, FAIL message absent."""
    _expect_quarantine_check(
        httpserver,
        response={
            "quarantined_tests_names": ["tests.test_func.test_failed"],
            "non_quarantined_tests_names": [],
        },
    )
    _expect_traces_upload(httpserver)

    result = cli(
        "ci",
        "junit-process",
        "--api-url",
        httpserver.url_for("").rstrip("/"),
        "--token",
        "test-token",
        "--repository",
        "owner/repo",
        "--tests-target-branch",
        "main",
        str(JUNIT_FAIL),
    )

    assert result.returncode == 0, f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    assert "quarantined" in result.stdout.lower()


def test_junit_process_failure_not_quarantined(
    httpserver: HTTPServer,
    cli: typing.Callable[..., typing.Any],
) -> None:
    """Failing test, not quarantined → exit 1."""
    _expect_quarantine_check(
        httpserver,
        response={
            "quarantined_tests_names": [],
            "non_quarantined_tests_names": ["tests.test_func.test_failed"],
        },
    )
    _expect_traces_upload(httpserver)

    result = cli(
        "ci",
        "junit-process",
        "--api-url",
        httpserver.url_for("").rstrip("/"),
        "--token",
        "test-token",
        "--repository",
        "owner/repo",
        "--tests-target-branch",
        "main",
        str(JUNIT_FAIL),
    )

    assert result.returncode == 1, f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    assert "FAIL" in result.stdout


def test_junit_process_quarantine_endpoint_error(
    httpserver: HTTPServer,
    cli: typing.Callable[..., typing.Any],
) -> None:
    """Quarantine endpoint 500 → failures treated as blocking, exit 1."""
    _expect_quarantine_check(
        httpserver,
        response={"quarantined_tests_names": [], "non_quarantined_tests_names": []},
        status=500,
    )
    _expect_traces_upload(httpserver)

    result = cli(
        "ci",
        "junit-process",
        "--api-url",
        httpserver.url_for("").rstrip("/"),
        "--token",
        "test-token",
        "--repository",
        "owner/repo",
        "--tests-target-branch",
        "main",
        str(JUNIT_FAIL),
    )

    assert result.returncode == 1
    assert "Failed to check quarantine" in result.stdout


def test_junit_upload_alias_still_works(
    httpserver: HTTPServer,
    cli: typing.Callable[..., typing.Any],
) -> None:
    """Deprecated `junit-upload` is a shim over `junit-process`."""
    _expect_quarantine_check(
        httpserver,
        response={"quarantined_tests_names": [], "non_quarantined_tests_names": []},
    )
    _expect_traces_upload(httpserver)

    result = cli(
        "ci",
        "junit-upload",
        "--api-url",
        httpserver.url_for("").rstrip("/"),
        "--token",
        "test-token",
        "--repository",
        "owner/repo",
        "--tests-target-branch",
        "main",
        str(JUNIT_PASS),
    )

    assert result.returncode == 0, f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
