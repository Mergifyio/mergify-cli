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
"""Shared fixtures for the live functional-test harness.

Tests in this directory drive the real `mergify` binary against
the real Mergify API. The `cli` fixture builds the invocation
with a clean environment (CI/GitHub/Buildkite vars scrubbed) so
runs are deterministic regardless of where they execute.
"""

from __future__ import annotations

import dataclasses
import os
import pathlib
import shutil
import subprocess
import typing


if typing.TYPE_CHECKING:
    from collections.abc import Mapping
    from collections.abc import Sequence

import pytest


# Environment variables that the CLI auto-detects from the surrounding
# CI runner. Scrub them so a developer running tests inside GitHub
# Actions / Buildkite doesn't get different behavior than a clean
# laptop run.
_CI_ENV_VARS = (
    "CI",
    "GITHUB_ACTIONS",
    "GITHUB_REPOSITORY",
    "GITHUB_REF",
    "GITHUB_HEAD_REF",
    "GITHUB_BASE_REF",
    "GITHUB_EVENT_PATH",
    "GITHUB_EVENT_NAME",
    "GITHUB_OUTPUT",
    "GITHUB_STEP_SUMMARY",
    "GITHUB_TOKEN",
    "BUILDKITE",
    "BUILDKITE_PULL_REQUEST",
    "BUILDKITE_PULL_REQUEST_BASE_BRANCH",
    "BUILDKITE_BRANCH",
    "BUILDKITE_COMMIT",
    "MERGIFY_API_URL",
    "MERGIFY_TOKEN",
    "MERGIFY_CONFIG_PATH",
    "MERGIFY_TEST_EXIT_CODE",
    "ACTIONS_STEP_DEBUG",
)


@dataclasses.dataclass(frozen=True)
class CliResult:
    returncode: int
    stdout: str
    stderr: str


@pytest.fixture
def live_token() -> str:
    """Skip the live test if `LIVE_TEST_MERGIFY_TOKEN` isn't set."""
    token = os.environ.get("LIVE_TEST_MERGIFY_TOKEN", "").strip()
    if not token:
        pytest.skip("LIVE_TEST_MERGIFY_TOKEN unset")
    return token


def _resolve_mergify_binary() -> pathlib.Path | None:
    """Locate the `mergify` binary in the active venv (or PATH)."""
    venv = os.environ.get("VIRTUAL_ENV")
    if venv:
        candidate = pathlib.Path(venv) / "bin" / "mergify"
        if candidate.exists():
            return candidate
        candidate = pathlib.Path(venv) / "Scripts" / "mergify.exe"
        if candidate.exists():
            return candidate
    found = shutil.which("mergify")
    return pathlib.Path(found) if found else None


@pytest.fixture(scope="session")
def mergify_binary() -> pathlib.Path:
    binary = _resolve_mergify_binary()
    if binary is None:
        pytest.skip(
            "`mergify` binary not found; run `uv sync` to install it",
        )
    return binary


@pytest.fixture
def cli(
    tmp_path: pathlib.Path,
    mergify_binary: pathlib.Path,
) -> typing.Callable[..., CliResult]:
    """Return a callable that runs `mergify <args>` in a subprocess.

    Runs from a fresh temp directory with CI-detection env vars
    scrubbed. Stdin is closed so any accidental interactive prompt
    fails fast instead of blocking. A 30s timeout caps pathological
    hangs.
    """

    def _run(
        *args: str,
        env: Mapping[str, str] | None = None,
        cwd: pathlib.Path | None = None,
    ) -> CliResult:
        full_env = {k: v for k, v in os.environ.items() if k not in _CI_ENV_VARS}
        if env:
            full_env.update(env)

        cmd: Sequence[str] = [str(mergify_binary), *args]
        proc = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            check=False,
            stdin=subprocess.DEVNULL,
            env=dict(full_env),
            cwd=str(cwd or tmp_path),
            timeout=30,
        )
        return CliResult(
            returncode=proc.returncode,
            stdout=proc.stdout,
            stderr=proc.stderr,
        )

    return _run
