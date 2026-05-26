"""Upload JUnit test results to Mergify CI Insights as OTLP spans.

The encode-and-upload step shells out to the native Rust binary's
hidden `_internal junit-upload` subcommand — it re-parses the
JUnit XML files, builds the OTLP `ExportTraceServiceRequest`
(with the caller-supplied quarantine set baked into each case
span's `cicd.test.quarantined` attribute), gzips the protobuf,
and POSTs it to `/v1/repos/<owner>/<repo>/ci/traces`.

This module is the Python→Rust migration bridge for the upload
step. When `ci junit-process` is fully ported to Rust (Phase C,
later in the same stack), this module, its callers, and the
`_internal junit-upload` Rust subcommand all go away.
"""

from __future__ import annotations

import subprocess
import typing

from mergify_cli.ci.junit_processing import junit


if typing.TYPE_CHECKING:
    from collections.abc import Iterable


class UploadError(Exception):
    pass


def upload(
    api_url: str,
    token: str,
    repository: str,
    files: tuple[str, ...],
    run_id: str,
    quarantined_names: Iterable[str] = (),
    test_framework: str | None = None,
    test_language: str | None = None,
    mergify_test_job_name: str | None = None,
) -> None:
    """Upload spans for `files` to Mergify CI Insights.

    `files` is the original tuple of paths passed to
    `process_junit_files`; the Rust binary re-parses them so the
    span attributes (suite name, classname-qualified test name,
    file/line, exception type/message/stacktrace) come straight
    from the same parser the Python side already used to compute
    the failing set.

    `run_id` must be the same 16-char hex identifier Python
    generated upstream (`junit.files_to_spans` returns it) so the
    session-span ID embedded in the upload matches what the CLI
    report later prints.

    `quarantined_names` is the set of test names the quarantine
    API said are currently quarantined. The Rust span builder
    sets `cicd.test.quarantined = true` on the matching case
    spans; everything else defaults to false.
    """
    if not files:
        return

    binary = junit._resolve_mergify_binary()
    cmd = [
        binary,
        "_internal",
        "junit-upload",
        "--api-url",
        api_url,
        "--token",
        token,
        "--repository",
        repository,
        "--run-id",
        run_id,
    ]
    if test_framework is not None:
        cmd.extend(["--test-framework", test_framework])
    if test_language is not None:
        cmd.extend(["--test-language", test_language])
    if mergify_test_job_name is not None:
        cmd.extend(["--mergify-test-job-name", mergify_test_job_name])
    for name in quarantined_names:
        cmd.extend(["--quarantined", name])
    cmd.extend(files)

    result = subprocess.run(  # noqa: S603
        cmd,
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        stderr = result.stderr.decode("utf-8", errors="replace").strip()
        # The Rust binary's generic error wrapper prefixes every
        # error with `mergify: ` — strip it so the message reads
        # cleanly when surfaced via UploadError.
        details = stderr.removeprefix("mergify: ")
        raise UploadError(details or "junit-upload failed")
