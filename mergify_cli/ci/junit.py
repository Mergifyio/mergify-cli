import dataclasses
import os
import pathlib
import time
import typing
from xml.etree import ElementTree as ET  # noqa: S405

from opentelemetry.sdk import resources
from opentelemetry.sdk.trace import ReadableSpan
from opentelemetry.sdk.trace.id_generator import RandomIdGenerator
from opentelemetry.semconv._incubating.attributes import (
    cicd_attributes,  # noqa: PLC2701
)
from opentelemetry.semconv._incubating.attributes import vcs_attributes  # noqa: PLC2701
from opentelemetry.semconv.trace import SpanAttributes
from opentelemetry.trace.propagation.tracecontext import TraceContextTextMapPropagator
import opentelemetry.trace.span

from mergify_cli import console
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
) -> list[ReadableSpan]:
    spans = []

    run_id = ID_GENERATOR.generate_span_id().to_bytes(8, "big").hex()
    console.print(f"🛠️ MERGIFY_TEST_RUN_ID={run_id}")

    for filename in files:
        spans.extend(
            await junit_to_spans(
                run_id,
                pathlib.Path(filename).read_bytes(),
                test_language=test_language,
                test_framework=test_framework,
            ),
        )

    return spans


async def junit_to_spans(
    run_id: str,
    xml_content: bytes,
    test_language: str | None = None,
    test_framework: str | None = None,
) -> list[ReadableSpan]:
    try:
        root = ET.fromstring(xml_content)  # noqa: S314
    except ET.ParseError as e:
        raise InvalidJunitXMLError(e.msg) from e

    # NOTE(sileht): We do the bare minimum for checking it's a valid Junit XML, without being super
    # strict on the format, as there is no official standard and at least 3 versions
    # in Junit itself, most implementations never implement 100% of the original format.

    if root.tag != "testsuites":
        msg = "no testsuites tag found"
        raise InvalidJunitXMLError(msg)

    testsuites = root.findall(".//{*}testsuite")
    if not testsuites:
        msg = "no testsuite tag found"
        raise InvalidJunitXMLError(msg)

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

    if (run_attempt := detector.get_cicd_pipeline_run_attempt()) is not None:
        resource_attributes["cicd.pipeline.run.attempt"] = run_attempt

    if (head_revision := (await detector.get_head_sha())) is not None:
        resource_attributes[vcs_attributes.VCS_REF_HEAD_REVISION] = head_revision

    if (head_ref_name := detector.get_head_ref_name()) is not None:
        resource_attributes[vcs_attributes.VCS_REF_HEAD_NAME] = head_ref_name

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

    spans = [session_span]

    for testsuite in testsuites:
        min_start_time = now
        suite_name = testsuite.get("name", "unnamed testsuite")

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

        for testcase in testsuite.findall("testcase"):
            classname = testcase.get("classname")
            if classname is not None:
                test_name = classname + "." + testcase.get("name", "unnamed test")
            else:
                test_name = testcase.get("name", "unnamed test")
            start_time = now - int(float(testcase.get("time", 0)) * 10e9)
            min_start_time = min(min_start_time, start_time)

            attributes: dict[str, str | bool] = {
                "test.scope": "case",
                "test.case.name": test_name,
                "code.function.name": test_name,
                "cicd.test.quarantined": False,
            }

            if (filename := testcase.get("file")) is not None:
                attributes[SpanAttributes.CODE_FILEPATH] = filename

            if (lineno := testcase.get("line")) is not None:
                attributes[SpanAttributes.CODE_LINENO] = lineno

            if testcase.find("skipped") is not None:
                attributes["test.case.result.status"] = "skipped"
                span_status = opentelemetry.trace.Status(
                    status_code=opentelemetry.trace.StatusCode.OK,
                )
            elif (
                testcase.find("failure") is not None
                or testcase.find("error") is not None
            ):
                attributes["test.case.result.status"] = "failed"
                span_status = opentelemetry.trace.Status(
                    status_code=opentelemetry.trace.StatusCode.ERROR,
                )

                for failed_conclusion in ("failure", "error"):
                    for failure in testcase.findall(failed_conclusion):
                        if (failure_type := failure.get("type")) is not None:
                            attributes[SpanAttributes.EXCEPTION_TYPE] = failure_type
                        if (failure_message := failure.get("message")) is not None:
                            attributes[SpanAttributes.EXCEPTION_MESSAGE] = (
                                failure_message
                            )
                        if failure.text is not None:
                            attributes[SpanAttributes.EXCEPTION_STACKTRACE] = (
                                failure.text.strip()
                            )
                        # We only care about the first failure/error
                        break
            else:
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
                attributes=common_attributes | attributes,
                status=span_status,
                resource=resource,
            )

            spans.append(span)

        testsuite_span._start_time = min_start_time  # noqa: SLF001
        session_start_time = min(session_start_time, min_start_time)

    session_span._start_time = session_start_time  # noqa: SLF001

    return spans
