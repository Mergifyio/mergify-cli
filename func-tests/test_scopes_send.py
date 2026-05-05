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
"""Functional tests for `mergify ci scopes-send`."""

from __future__ import annotations

import json
import typing


if typing.TYPE_CHECKING:
    import pathlib

    from pytest_httpserver import HTTPServer


def test_scopes_send_posts_direct_scopes(
    httpserver: HTTPServer,
    cli: typing.Callable[..., typing.Any],
) -> None:
    httpserver.expect_oneshot_request(
        "/v1/repos/owner/repo/pulls/42/scopes",
        method="POST",
        headers={"Authorization": "Bearer test-token"},
        json={"scopes": ["backend"]},
    ).respond_with_data("", status=200)

    result = cli(
        "ci",
        "scopes-send",
        "--api-url",
        httpserver.url_for("").rstrip("/"),
        "--token",
        "test-token",
        "--repository",
        "owner/repo",
        "--pull-request",
        "42",
        "--scope",
        "backend",
    )

    assert result.returncode == 0, f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    httpserver.check_assertions()


def test_scopes_send_combines_flags_json_and_text_file(
    httpserver: HTTPServer,
    cli: typing.Callable[..., typing.Any],
    tmp_path: pathlib.Path,
) -> None:
    json_path = tmp_path / "scopes.json"
    json_path.write_text(json.dumps({"scopes": ["fromjson"]}))

    txt_path = tmp_path / "scopes.txt"
    txt_path.write_text("fromtext\n")

    httpserver.expect_oneshot_request(
        "/v1/repos/owner/repo/pulls/7/scopes",
        method="POST",
        json={"scopes": ["direct", "fromjson", "fromtext"]},
    ).respond_with_data("", status=200)

    result = cli(
        "ci",
        "scopes-send",
        "--api-url",
        httpserver.url_for("").rstrip("/"),
        "--token",
        "t",
        "--repository",
        "owner/repo",
        "--pull-request",
        "7",
        "--scope",
        "direct",
        "--scopes-json",
        str(json_path),
        "--scopes-file",
        str(txt_path),
    )

    assert result.returncode == 0, f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    httpserver.check_assertions()


def test_scopes_send_skips_when_no_pull_request(
    httpserver: HTTPServer,
    cli: typing.Callable[..., typing.Any],
) -> None:
    """No PR detected (no flag, no GITHUB_EVENT_PATH) → clean skip, no HTTP."""
    result = cli(
        "ci",
        "scopes-send",
        "--api-url",
        httpserver.url_for("").rstrip("/"),
        "--token",
        "t",
        "--repository",
        "owner/repo",
        "--scope",
        "backend",
    )

    assert result.returncode == 0, f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    # No request should have been made.
    assert len(httpserver.log) == 0, (
        f"unexpected requests: {[r[0].path for r in httpserver.log]}"
    )


def test_scopes_send_propagates_server_error(
    httpserver: HTTPServer,
    cli: typing.Callable[..., typing.Any],
) -> None:
    httpserver.expect_oneshot_request(
        "/v1/repos/owner/repo/pulls/1/scopes",
        method="POST",
    ).respond_with_data("forbidden", status=403)

    result = cli(
        "ci",
        "scopes-send",
        "--api-url",
        httpserver.url_for("").rstrip("/"),
        "--token",
        "t",
        "--repository",
        "owner/repo",
        "--pull-request",
        "1",
        "--scope",
        "backend",
    )

    assert result.returncode != 0
    httpserver.check_assertions()
