"""Parse JUnit XML and build OTLP spans for the CI Insights upload.

The XML parsing step shells out to the native Rust binary's hidden
`_internal junit-parse` subcommand — it returns a list of typed
test cases with the same fields the inline `xml.etree` walk used
to read off ET nodes (suite name, classname-qualified test name,
duration, file/line, status, exception kind/message/stacktrace).

This module is the Python→Rust migration bridge for `junit-process`.
When `ci junit-process` is fully ported to Rust (Phase C, planned
later in the same stack), this module, its callers, and the
`_internal junit-parse` Rust subcommand all go away.
"""

from __future__ import annotations

import dataclasses
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time
import typing

from opentelemetry.sdk import resources
from opentelemetry.sdk.trace import ReadableSpan
from opentelemetry.sdk.trace.id_generator import RandomIdGenerator
from opentelemetry.semconv._incubating.attributes import (  # noqa: PLC2701
    cicd_attributes,
)
from opentelemetry.semconv._incubating.attributes import vcs_attributes  # noqa: PLC2701
from opentelemetry.semconv.trace import SpanAttributes
from opentelemetry.trace.propagation.tracecontext import TraceContextTextMapPropagator
import opentelemetry.trace.span

from mergify_cli.ci import detector


if typing.TYPE_CHECKING:
    from opentelemetry.trace import NonRecordingSpan

ID_GENERATOR = RandomIdGenerator()


@dataclasses.dataclass
class InvalidJunitXMLError(Exception):
    details: str


async def files_to_spans(
    files: tuple[str, ...],
    test_language: str | None = None,
    test_framework: str | None = None,
) -> tuple[str, list[ReadableSpan]]:
    spans = []

    run_id = ID_GENERATOR.generate_span_id().to_bytes(8, "big").hex()

    for filename in files:
        spans.extend(
            await junit_to_spans(
                run_id,
                pathlib.Path(filename).read_bytes(),
                test_language=test_language,
                test_framework=test_framework,
            ),
        )

    return run_id, spans


async def junit_to_spans(
    run_id: str,
    xml_content: bytes,
    test_language: str | None = None,
    test_framework: str | None = None,
) -> list[ReadableSpan]:
    parsed = _parse_with_rust(xml_content)
    suite_names = parsed["suite_names"]
    cases = parsed["cases"]

    # The native parser raises on an unknown root tag and on a
    # `<testsuites>` / `<testsuite>` root with zero descendants, so
    # by the time we get here `suite_names` is non-empty in every
    # valid parse. Keep the explicit check anyway — a future parser
    # change that emits an empty list shouldn't silently produce
    # an OTLP request with no spans.
    if not suite_names:
        raise InvalidJunitXMLError("no testsuites or testsuite tag found")

    now = time.time_ns()

    common_attributes: dict[str, str | bool] = {}

    if test_framework is not None:
        common_attributes["test.framework"] = test_framework

    if test_language is not None:
        common_attributes["test.language"] = test_language

    resource_attributes: dict[str, typing.Any] = {
        "test.run.id": run_id,
    }

    if "MERGIFY_TEST_JOB_NAME" in os.environ:
        resource_attributes["mergify.test.job.name"] = os.environ[
            "MERGIFY_TEST_JOB_NAME"
        ]

    if (pipeline_name := detector.get_pipeline_name()) is not None:
        resource_attributes[cicd_attributes.CICD_PIPELINE_NAME] = pipeline_name

    if (job_name := detector.get_job_name()) is not None:
        resource_attributes[cicd_attributes.CICD_PIPELINE_TASK_NAME] = job_name

    if (cicd_run_id := detector.get_cicd_pipeline_run_id()) is not None:
        resource_attributes[cicd_attributes.CICD_PIPELINE_RUN_ID] = cicd_run_id

    if (run_url := detector.get_cicd_pipeline_run_url()) is not None:
        resource_attributes["cicd.pipeline.run.url"] = run_url

    if (run_attempt := detector.get_cicd_pipeline_run_attempt()) is not None:
        resource_attributes["cicd.pipeline.run.attempt"] = run_attempt

    if (head_revision := (await detector.get_head_sha())) is not None:
        resource_attributes[vcs_attributes.VCS_REF_HEAD_REVISION] = head_revision

    if (head_ref_name := detector.get_head_ref_name()) is not None:
        resource_attributes[vcs_attributes.VCS_REF_HEAD_NAME] = head_ref_name

    if (base_ref_name := detector.get_base_ref_name()) is not None:
        resource_attributes[vcs_attributes.VCS_REF_BASE_NAME] = base_ref_name

    if (repo_url := detector.get_repository_url()) is not None:
        resource_attributes[vcs_attributes.VCS_REPOSITORY_URL_FULL] = repo_url

    if (repo_name := detector.get_github_repository()) is not None:
        resource_attributes["vcs.repository.name"] = repo_name

    if (
        cicd_pipeline_runner_name := detector.get_cicd_pipeline_runner_name()
    ) is not None:
        resource_attributes["cicd.pipeline.runner.name"] = cicd_pipeline_runner_name

    if (provider := detector.get_ci_provider()) is not None:
        resource_attributes["cicd.provider.name"] = provider

    resource = resources.Resource.create(resource_attributes)

    traceparent = os.environ.get("MERGIFY_TRACEPARENT")
    if traceparent:
        parent_context = TraceContextTextMapPropagator().extract(
            carrier={"traceparent": traceparent},
        )
        parent_span = typing.cast(
            "NonRecordingSpan",
            next(iter(parent_context.values())),
        )
        parent = parent_span.get_span_context()
        trace_id = parent.trace_id
    else:
        trace_id = ID_GENERATOR.generate_trace_id()
        parent = None

    session_context = opentelemetry.trace.span.SpanContext(
        trace_id=trace_id,
        span_id=ID_GENERATOR.generate_span_id(),
        is_remote=False,
    )

    session_span = ReadableSpan(
        name="test session",
        context=session_context,
        parent=parent,
        # We'll compute start_time later
        end_time=now,
        resource=resource,
        attributes=common_attributes
        | {
            "test.scope": "session",
        },
    )

    session_start_time = now

    spans: list[ReadableSpan] = [session_span]

    # Index cases by suite name. The Rust parser tagged each case
    # with the closest enclosing suite, so cases land in the right
    # bucket even when suites nest. Order of iteration over the
    # buckets is driven by `suite_names` (document order from the
    # parser), not by first-seen case — a nested suite's cases
    # appear in the case stream before the parent suite's *direct*
    # cases, but the parent's span has to come first.
    grouped_cases: dict[str, list[dict[str, typing.Any]]] = {}
    for case in cases:
        grouped_cases.setdefault(case["suite_name"], []).append(case)

    for suite_name in suite_names:
        suite_cases = grouped_cases.get(suite_name, [])
        min_start_time = now

        testsuite_context = opentelemetry.trace.span.SpanContext(
            trace_id=trace_id,
            span_id=ID_GENERATOR.generate_span_id(),
            is_remote=False,
        )

        testsuite_span = ReadableSpan(
            name=suite_name,
            context=testsuite_context,
            parent=session_context,
            # We'll compute start_time later
            end_time=now,
            resource=resource,
            attributes=common_attributes
            | {
                "test.case.name": suite_name,
                "test.scope": "suite",
            },
        )

        spans.append(testsuite_span)

        for case in suite_cases:
            test_name = case["name"]
            duration_secs = case["duration"]
            start_time = (
                now if duration_secs is None else now - int(float(duration_secs) * 10e9)
            )
            min_start_time = min(min_start_time, start_time)

            attributes: dict[str, str | bool] = {
                "test.scope": "case",
                "test.case.name": test_name,
                "code.function.name": test_name,
                "cicd.test.quarantined": False,
            }

            if case["file"] is not None:
                attributes[SpanAttributes.CODE_FILEPATH] = case["file"]
            if case["line"] is not None:
                attributes[SpanAttributes.CODE_LINENO] = case["line"]

            status = case["status"]
            if status == "skipped":
                attributes["test.case.result.status"] = "skipped"
                span_status = opentelemetry.trace.Status(
                    status_code=opentelemetry.trace.StatusCode.OK,
                )
            elif status in {"failed", "errored"}:
                attributes["test.case.result.status"] = "failed"
                span_status = opentelemetry.trace.Status(
                    status_code=opentelemetry.trace.StatusCode.ERROR,
                )
                failure = case["failure"]
                if failure["kind"]:
                    attributes[SpanAttributes.EXCEPTION_TYPE] = failure["kind"]
                if failure["message"]:
                    attributes[SpanAttributes.EXCEPTION_MESSAGE] = failure["message"]
                if failure["stacktrace"]:
                    attributes[SpanAttributes.EXCEPTION_STACKTRACE] = failure[
                        "stacktrace"
                    ]
            else:  # "passed"
                attributes["test.case.result.status"] = "passed"
                span_status = opentelemetry.trace.Status(
                    status_code=opentelemetry.trace.StatusCode.OK,
                )

            span = ReadableSpan(
                name=test_name,
                start_time=start_time,
                end_time=now,
                context=opentelemetry.trace.span.SpanContext(
                    trace_id=trace_id,
                    span_id=ID_GENERATOR.generate_span_id(),
                    is_remote=False,
                ),
                parent=testsuite_context,
                attributes={**common_attributes, **attributes},
                status=span_status,
                resource=resource,
            )

            spans.append(span)

        testsuite_span._start_time = min_start_time
        session_start_time = min(session_start_time, min_start_time)

    session_span._start_time = session_start_time

    return spans


def _parse_with_rust(xml_content: bytes) -> dict[str, typing.Any]:
    """Shell out to `mergify _internal junit-parse` for XML parsing.

    The native parser handles the loose JUnit dialect (nested
    `<testsuites>`, bare `<testsuite>`, namespaces) and emits a
    JSON object `{"suite_names": [...], "cases": [...]}` where
    `suite_names` lists suites in document order and `cases` is a
    flat array of typed test cases each tagged with the closest
    enclosing suite name. Errors from the subprocess (invalid XML,
    binary not found, unreadable file) surface as
    `InvalidJunitXMLError` so callers can fail the same way they
    used to when `ET.fromstring` raised.
    """
    binary = _resolve_mergify_binary()
    # The subcommand takes a path rather than reading stdin so the
    # CLI surface stays minimal. Write the bytes to a temp file
    # and pass the path.
    with tempfile.NamedTemporaryFile(suffix=".xml", delete=False) as tmp:
        tmp.write(xml_content)
        tmp_path = tmp.name
    try:
        result = subprocess.run(  # noqa: S603
            [binary, "_internal", "junit-parse", tmp_path],
            capture_output=True,
            check=False,
        )
    finally:
        pathlib.Path(tmp_path).unlink(missing_ok=True)
    if result.returncode != 0:
        stderr = result.stderr.decode("utf-8", errors="replace").strip()
        # The Rust binary's generic error wrapper prefixes every
        # error with `mergify: `, and `InvalidJunitXml`'s Display
        # impl prefixes the parse-specific details with
        # `Failed to parse JUnit XML: `. Strip both so callers
        # (notably `process_junit_files`, which re-wraps with
        # `f"Failed to parse JUnit XML: {e.details}"`) don't end
        # up double-prefixing the user-facing message.
        details = stderr.removeprefix("mergify: ").removeprefix(
            "Failed to parse JUnit XML: ",
        )
        raise InvalidJunitXMLError(details or "junit-parse failed")
    return typing.cast("dict[str, typing.Any]", json.loads(result.stdout))


def _resolve_mergify_binary() -> str:
    """Locate the `mergify` Rust binary the Python side calls into.

    Lookup order:
    1. `$MERGIFY_CLI_BIN` override (used by tests against a freshly
       built `target/debug/mergify`).
    2. The venv's `bin/mergify` next to `sys.executable` — this is
       the production case once the wheel is installed.
    3. `$PATH` fallback via `shutil.which`.
    """
    if override := os.environ.get("MERGIFY_CLI_BIN"):
        return override
    venv_bin = pathlib.Path(sys.executable).parent
    for name in ("mergify", "mergify.exe"):
        candidate = venv_bin / name
        if candidate.is_file():
            return str(candidate)
    if found := shutil.which("mergify"):
        return found
    msg = (
        "could not locate the `mergify` binary "
        f"(checked $MERGIFY_CLI_BIN, {venv_bin}/mergify, and $PATH)"
    )
    raise RuntimeError(msg)
