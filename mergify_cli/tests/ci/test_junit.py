import json
import pathlib
from unittest import mock

import anys
import opentelemetry.trace.span

from mergify_cli.ci import detector
from mergify_cli.ci import junit


@mock.patch.object(detector, "get_ci_provider", return_value="github_actions")
@mock.patch.object(detector, "get_job_name", return_value="JOB")
@mock.patch.object(detector, "get_cicd_pipeline_run_id", return_value=123)
@mock.patch.object(detector, "get_cicd_pipeline_run_attempt", return_value=1)
@mock.patch.object(
    detector,
    "get_head_sha",
    return_value="3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
)
@mock.patch.object(
    detector,
    "get_head_ref_name",
    return_value="refs/heads/main",
)
async def test_parse(
    _get_ci_provider: mock.Mock,
    _get_job_name: mock.Mock,
    _get_cicd_pipeline_run_id: mock.Mock,
    _get_cicd_pipeline_run_attempt: mock.Mock,
    _get_head_sha: mock.Mock,
    _get_head_ref_name: mock.Mock,
) -> None:
    filename = pathlib.Path(__file__).parent / "junit_example.xml"
    run_id = (32312).to_bytes(8, "big").hex()
    spans = await junit.junit_to_spans(
        run_id,
        filename.read_bytes(),
        "python",
        "unittest",
    )
    dictified_spans = [json.loads(span.to_json()) for span in spans]
    trace_id = "0x" + opentelemetry.trace.span.format_trace_id(
        spans[1].context.trace_id,
    )
    resource_attributes = {
        "test.run.id": run_id,
        "cicd.pipeline.name": "JOB",
        "cicd.pipeline.run.id": 123,
        "cicd.pipeline.run.attempt": 1,
        "cicd.provider.name": "github_actions",
        "vcs.ref.head.revision": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
        "vcs.ref.head.name": "refs/heads/main",
        "service.name": "unknown_service",
        "telemetry.sdk.language": "python",
        "telemetry.sdk.name": "opentelemetry",
        "telemetry.sdk.version": anys.ANY_STR,
    }
    assert dictified_spans == [
        {
            "attributes": {
                "test.case.name": "Tests.Registration",
                "test.scope": "suite",
                "test.framework": "unittest",
                "test.language": "python",
            },
            "context": {
                "span_id": anys.ANY_STR,
                "trace_id": trace_id,
                "trace_state": "[]",
            },
            "end_time": anys.ANY_DATETIME_STR,
            "events": [],
            "kind": "SpanKind.INTERNAL",
            "links": [],
            "name": "Tests.Registration",
            "parent_id": None,
            "resource": {
                "attributes": resource_attributes,
                "schema_url": "",
            },
            "start_time": anys.ANY_DATETIME_STR,
            "status": {
                "status_code": "UNSET",
            },
        },
        {
            "attributes": {
                "test.case.name": "Tests.Registration.testCase1",
                "test.case.result.status": "passed",
                "test.scope": "case",
                "test.framework": "unittest",
                "test.language": "python",
            },
            "context": {
                "span_id": anys.ANY_STR,
                "trace_id": trace_id,
                "trace_state": "[]",
            },
            "end_time": anys.ANY_DATETIME_STR,
            "events": [],
            "kind": "SpanKind.INTERNAL",
            "links": [],
            "name": "Tests.Registration.testCase1",
            "parent_id": anys.ANY_STR,
            "resource": {
                "attributes": resource_attributes,
                "schema_url": "",
            },
            "start_time": anys.ANY_DATETIME_STR,
            "status": {
                "status_code": "OK",
            },
        },
        {
            "attributes": {
                "test.case.name": "Tests.Registration.testCase2",
                "test.case.result.status": "skipped",
                "test.scope": "case",
                "test.framework": "unittest",
                "test.language": "python",
            },
            "context": {
                "span_id": anys.ANY_STR,
                "trace_id": trace_id,
                "trace_state": "[]",
            },
            "end_time": anys.ANY_DATETIME_STR,
            "events": [],
            "kind": "SpanKind.INTERNAL",
            "links": [],
            "name": "Tests.Registration.testCase2",
            "parent_id": anys.ANY_STR,
            "resource": {
                "attributes": resource_attributes,
                "schema_url": "",
            },
            "start_time": anys.ANY_DATETIME_STR,
            "status": {
                "status_code": "OK",
            },
        },
        {
            "attributes": {
                "exception.message": "invalid literal for int() with base 10: 'foobar'",
                "exception.stacktrace": "bip, bip, bip, error!",
                "exception.type": "ValueError",
                "test.case.name": "Tests.Registration.testCase3",
                "test.case.result.status": "failed",
                "test.scope": "case",
                "test.framework": "unittest",
                "test.language": "python",
            },
            "context": {
                "span_id": anys.ANY_STR,
                "trace_id": trace_id,
                "trace_state": "[]",
            },
            "end_time": anys.ANY_DATETIME_STR,
            "events": [],
            "kind": "SpanKind.INTERNAL",
            "links": [],
            "name": "Tests.Registration.testCase3",
            "parent_id": anys.ANY_STR,
            "resource": {
                "attributes": resource_attributes,
                "schema_url": "",
            },
            "start_time": anys.ANY_DATETIME_STR,
            "status": {
                "status_code": "ERROR",
            },
        },
        {
            "attributes": {
                "test.case.name": "Tests.Authentication",
                "test.scope": "suite",
                "test.framework": "unittest",
                "test.language": "python",
            },
            "context": {
                "span_id": anys.ANY_STR,
                "trace_id": trace_id,
                "trace_state": "[]",
            },
            "end_time": anys.ANY_DATETIME_STR,
            "events": [],
            "kind": "SpanKind.INTERNAL",
            "links": [],
            "name": "Tests.Authentication",
            "parent_id": None,
            "resource": {
                "attributes": resource_attributes,
                "schema_url": "",
            },
            "start_time": anys.ANY_DATETIME_STR,
            "status": {
                "status_code": "UNSET",
            },
        },
        {
            "attributes": {
                "test.case.name": "Tests.Authentication.testCase7",
                "test.case.result.status": "passed",
                "test.scope": "case",
                "test.framework": "unittest",
                "test.language": "python",
            },
            "context": {
                "span_id": anys.ANY_STR,
                "trace_id": trace_id,
                "trace_state": "[]",
            },
            "end_time": anys.ANY_DATETIME_STR,
            "events": [],
            "kind": "SpanKind.INTERNAL",
            "links": [],
            "name": "Tests.Authentication.testCase7",
            "parent_id": anys.ANY_STR,
            "resource": {
                "attributes": resource_attributes,
                "schema_url": "",
            },
            "start_time": anys.ANY_DATETIME_STR,
            "status": {
                "status_code": "OK",
            },
        },
        {
            "attributes": {
                "test.case.name": "Tests.Authentication.testCase8",
                "test.case.result.status": "passed",
                "test.scope": "case",
                "test.framework": "unittest",
                "test.language": "python",
            },
            "context": {
                "span_id": anys.ANY_STR,
                "trace_id": trace_id,
                "trace_state": "[]",
            },
            "end_time": anys.ANY_DATETIME_STR,
            "events": [],
            "kind": "SpanKind.INTERNAL",
            "links": [],
            "name": "Tests.Authentication.testCase8",
            "parent_id": anys.ANY_STR,
            "resource": {
                "attributes": resource_attributes,
                "schema_url": "",
            },
            "start_time": anys.ANY_DATETIME_STR,
            "status": {
                "status_code": "OK",
            },
        },
        {
            "attributes": {
                "exception.message": "Assertion error message",
                "exception.stacktrace": "Such a mess, the failure is unrecoverable",
                "exception.type": "AssertionError",
                "test.case.name": "Tests.Authentication.testCase9",
                "test.case.result.status": "failed",
                "test.scope": "case",
                "test.framework": "unittest",
                "test.language": "python",
            },
            "context": {
                "span_id": anys.ANY_STR,
                "trace_id": trace_id,
                "trace_state": "[]",
            },
            "end_time": anys.ANY_DATETIME_STR,
            "events": [],
            "kind": "SpanKind.INTERNAL",
            "links": [],
            "name": "Tests.Authentication.testCase9",
            "parent_id": anys.ANY_STR,
            "resource": {
                "attributes": resource_attributes,
                "schema_url": "",
            },
            "start_time": anys.ANY_DATETIME_STR,
            "status": {
                "status_code": "ERROR",
            },
        },
        {
            "attributes": {
                "exception.message": "division by zero",
                "exception.stacktrace": "Everything is broken, meh!\n"
                "With a second line!",
                "exception.type": "ZeroDivisionError",
                "test.case.name": "Tests.Permission.testCase10",
                "test.case.result.status": "failed",
                "test.scope": "case",
                "test.framework": "unittest",
                "test.language": "python",
            },
            "context": {
                "span_id": anys.ANY_STR,
                "trace_id": trace_id,
                "trace_state": "[]",
            },
            "end_time": anys.ANY_DATETIME_STR,
            "events": [],
            "kind": "SpanKind.INTERNAL",
            "links": [],
            "name": "Tests.Permission.testCase10",
            "parent_id": anys.ANY_STR,
            "resource": {
                "attributes": resource_attributes,
                "schema_url": "",
            },
            "start_time": anys.ANY_DATETIME_STR,
            "status": {
                "status_code": "ERROR",
            },
        },
        {
            "attributes": {
                "test.case.name": "Tests.Authentication.Login",
                "test.scope": "suite",
                "test.framework": "unittest",
                "test.language": "python",
            },
            "context": {
                "span_id": anys.ANY_STR,
                "trace_id": trace_id,
                "trace_state": "[]",
            },
            "end_time": anys.ANY_DATETIME_STR,
            "events": [],
            "kind": "SpanKind.INTERNAL",
            "links": [],
            "name": "Tests.Authentication.Login",
            "parent_id": None,
            "resource": {
                "attributes": resource_attributes,
                "schema_url": "",
            },
            "start_time": anys.ANY_DATETIME_STR,
            "status": {
                "status_code": "UNSET",
            },
        },
        {
            "attributes": {
                "test.case.name": "Tests.Authentication.Login.testCase4",
                "test.case.result.status": "passed",
                "test.scope": "case",
                "test.framework": "unittest",
                "test.language": "python",
            },
            "context": {
                "span_id": anys.ANY_STR,
                "trace_id": trace_id,
                "trace_state": "[]",
            },
            "end_time": anys.ANY_DATETIME_STR,
            "events": [],
            "kind": "SpanKind.INTERNAL",
            "links": [],
            "name": "Tests.Authentication.Login.testCase4",
            "parent_id": anys.ANY_STR,
            "resource": {
                "attributes": resource_attributes,
                "schema_url": "",
            },
            "start_time": anys.ANY_DATETIME_STR,
            "status": {
                "status_code": "OK",
            },
        },
        {
            "attributes": {
                "exception.message": "invalid syntax",
                "exception.stacktrace": "bad syntax, bad!",
                "exception.type": "SyntaxError",
                "test.case.name": "Tests.Authentication.Login.testCase5",
                "test.case.result.status": "failed",
                "test.scope": "case",
                "test.framework": "unittest",
                "test.language": "python",
            },
            "context": {
                "span_id": anys.ANY_STR,
                "trace_id": trace_id,
                "trace_state": "[]",
            },
            "end_time": anys.ANY_DATETIME_STR,
            "events": [],
            "kind": "SpanKind.INTERNAL",
            "links": [],
            "name": "Tests.Authentication.Login.testCase5",
            "parent_id": anys.ANY_STR,
            "resource": {
                "attributes": resource_attributes,
                "schema_url": "",
            },
            "start_time": anys.ANY_DATETIME_STR,
            "status": {
                "status_code": "ERROR",
            },
        },
        {
            "attributes": {
                "test.case.name": "Tests.Authentication.Login.testCase6",
                "test.case.result.status": "passed",
                "test.scope": "case",
                "test.framework": "unittest",
                "test.language": "python",
            },
            "context": {
                "span_id": anys.ANY_STR,
                "trace_id": trace_id,
                "trace_state": "[]",
            },
            "end_time": anys.ANY_DATETIME_STR,
            "events": [],
            "kind": "SpanKind.INTERNAL",
            "links": [],
            "name": "Tests.Authentication.Login.testCase6",
            "parent_id": anys.ANY_STR,
            "resource": {
                "attributes": resource_attributes,
                "schema_url": "",
            },
            "start_time": anys.ANY_DATETIME_STR,
            "status": {
                "status_code": "OK",
            },
        },
    ]
