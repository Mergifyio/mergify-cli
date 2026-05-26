"""Tests for the `upload` bridge that shells out to
`mergify _internal junit-upload`.

The actual HTTP upload behavior is covered by the Rust-side
wiremock tests in `crates/mergify-ci/src/junit_process/upload.rs`
(happy path, empty request short-circuit, 401 error surface).
These Python tests focus on the subprocess wiring: that the right
binary gets invoked with the right flag set, and that non-zero
exit codes surface as `UploadError` with the prefix stripped.
"""

from __future__ import annotations

import pathlib
import subprocess
from unittest import mock

import pytest

from mergify_cli.ci.junit_processing import upload


FIXTURES_DIR = pathlib.Path(__file__).parent.parent / "fixtures"
REPORT_XML = FIXTURES_DIR / "report.xml"


def _completed(
    returncode: int = 0,
    stderr: bytes = b"",
) -> subprocess.CompletedProcess[bytes]:
    return subprocess.CompletedProcess(
        args=[],
        returncode=returncode,
        stdout=b"",
        stderr=stderr,
    )


def test_upload_invokes_subcommand_with_all_metadata() -> None:
    with (
        mock.patch.object(
            upload.junit, "_resolve_mergify_binary", return_value="/bin/mergify"
        ),
        mock.patch.object(
            upload.subprocess,
            "run",
            return_value=_completed(),
        ) as run_mock,
    ):
        upload.upload(
            api_url="https://api.mergify.com",
            token="secret",
            repository="user/repo",
            files=(str(REPORT_XML), "other.xml"),
            run_id="0011223344556677",
            quarantined_names=["test_a", "test_b"],
            test_framework="pytest",
            test_language="python",
            mergify_test_job_name="ci-job",
        )

    run_mock.assert_called_once()
    cmd = run_mock.call_args.args[0]
    assert cmd == [
        "/bin/mergify",
        "_internal",
        "junit-upload",
        "--api-url",
        "https://api.mergify.com",
        "--token",
        "secret",
        "--repository",
        "user/repo",
        "--run-id",
        "0011223344556677",
        "--test-framework",
        "pytest",
        "--test-language",
        "python",
        "--mergify-test-job-name",
        "ci-job",
        "--quarantined",
        "test_a",
        "--quarantined",
        "test_b",
        str(REPORT_XML),
        "other.xml",
    ]


def test_upload_omits_optional_flags_when_unset() -> None:
    with (
        mock.patch.object(
            upload.junit, "_resolve_mergify_binary", return_value="/bin/mergify"
        ),
        mock.patch.object(
            upload.subprocess,
            "run",
            return_value=_completed(),
        ) as run_mock,
    ):
        upload.upload(
            api_url="https://api.mergify.com",
            token="secret",
            repository="user/repo",
            files=(str(REPORT_XML),),
            run_id="0011223344556677",
        )

    cmd = run_mock.call_args.args[0]
    # No --test-framework / --test-language / --mergify-test-job-name
    # / --quarantined flags when the caller didn't supply them.
    for flag in (
        "--test-framework",
        "--test-language",
        "--mergify-test-job-name",
        "--quarantined",
    ):
        assert flag not in cmd, f"{flag!r} should not appear in cmd: {cmd!r}"


def test_upload_short_circuits_on_empty_files() -> None:
    # No files → no subprocess. Mirrors the Python pre-Rust
    # behavior where `upload.upload` returned early on empty
    # spans, saving a no-op round trip.
    with mock.patch.object(upload.subprocess, "run") as run_mock:
        upload.upload(
            api_url="https://api.mergify.com",
            token="secret",
            repository="user/repo",
            files=(),
            run_id="0011223344556677",
        )
    run_mock.assert_not_called()


def test_upload_surfaces_subprocess_failure_with_prefix_stripped() -> None:
    with (
        mock.patch.object(
            upload.junit, "_resolve_mergify_binary", return_value="/bin/mergify"
        ),
        mock.patch.object(
            upload.subprocess,
            "run",
            return_value=_completed(
                returncode=1,
                stderr=b"mergify: HTTP 422: Not enabled on this repository\n",
            ),
        ),
        pytest.raises(upload.UploadError, match="HTTP 422"),
    ):
        upload.upload(
            api_url="https://api.mergify.com",
            token="secret",
            repository="user/repo",
            files=(str(REPORT_XML),),
            run_id="0011223344556677",
        )
