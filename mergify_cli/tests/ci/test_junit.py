import json
import pathlib
from unittest import mock

import anys
import opentelemetry.trace.span

from mergify_cli.ci import detector
from mergify_cli.ci import junit


@mock.patch.object(detector, "get_ci_provider", return_value="github_actions")
@mock.patch.object(detector, "get_job_name", return_value="JOB")
@mock.patch.object(
    detector,
    "get_head_sha",
    return_value="3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
)
async def test_parse(
    _get_ci_provider: mock.Mock,
    _get_job_name: mock.Mock,
    _get_head_sha: mock.Mock,
) -> None:
    filename = pathlib.Path(__file__).parent / "junit_example.xml"
    spans = await junit.junit_to_spans(
        123,
        filename.read_bytes(),
        "python",
        "unittest",
    )
    dictified_spans = [json.loads(span.to_json()) for span in spans]
    trace_id = "0x" + opentelemetry.trace.span.format_trace_id(
        spans[1].context.trace_id,
    )
    assert dictified_spans == [
        {
            "attributes": {
                "test.case.name": "Tests.Registration",
                "test.type": "suite",
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
                "attributes": {
                    "cicd.pipeline.name": "JOB",
                    "cicd.provider.name": "github_actions",
                    "vcs.ref.head.revision": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
                    "service.name": "unknown_service",
                    "telemetry.sdk.language": "python",
                    "telemetry.sdk.name": "opentelemetry",
                    "telemetry.sdk.version": anys.ANY_STR,
                },
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
                "test.case.result.status": "success",
                "test.type": "case",
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
                "attributes": {
                    "cicd.pipeline.name": "JOB",
                    "cicd.provider.name": "github_actions",
                    "vcs.ref.head.revision": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
                    "service.name": "unknown_service",
                    "telemetry.sdk.language": "python",
                    "telemetry.sdk.name": "opentelemetry",
                    "telemetry.sdk.version": anys.ANY_STR,
                },
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
                "test.type": "case",
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
                "attributes": {
                    "cicd.pipeline.name": "JOB",
                    "cicd.provider.name": "github_actions",
                    "vcs.ref.head.revision": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
                    "service.name": "unknown_service",
                    "telemetry.sdk.language": "python",
                    "telemetry.sdk.name": "opentelemetry",
                    "telemetry.sdk.version": anys.ANY_STR,
                },
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
                "test.case.result.status": "failure",
                "test.type": "case",
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
                "attributes": {
                    "cicd.pipeline.name": "JOB",
                    "cicd.provider.name": "github_actions",
                    "vcs.ref.head.revision": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
                    "service.name": "unknown_service",
                    "telemetry.sdk.language": "python",
                    "telemetry.sdk.name": "opentelemetry",
                    "telemetry.sdk.version": anys.ANY_STR,
                },
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
                "test.type": "suite",
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
                "attributes": {
                    "cicd.pipeline.name": "JOB",
                    "cicd.provider.name": "github_actions",
                    "vcs.ref.head.revision": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
                    "service.name": "unknown_service",
                    "telemetry.sdk.language": "python",
                    "telemetry.sdk.name": "opentelemetry",
                    "telemetry.sdk.version": anys.ANY_STR,
                },
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
                "test.case.result.status": "success",
                "test.type": "case",
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
                "attributes": {
                    "cicd.pipeline.name": "JOB",
                    "cicd.provider.name": "github_actions",
                    "vcs.ref.head.revision": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
                    "service.name": "unknown_service",
                    "telemetry.sdk.language": "python",
                    "telemetry.sdk.name": "opentelemetry",
                    "telemetry.sdk.version": anys.ANY_STR,
                },
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
                "test.case.result.status": "success",
                "test.type": "case",
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
                "attributes": {
                    "cicd.pipeline.name": "JOB",
                    "cicd.provider.name": "github_actions",
                    "vcs.ref.head.revision": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
                    "service.name": "unknown_service",
                    "telemetry.sdk.language": "python",
                    "telemetry.sdk.name": "opentelemetry",
                    "telemetry.sdk.version": anys.ANY_STR,
                },
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
                "test.case.result.status": "failure",
                "test.type": "case",
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
                "attributes": {
                    "cicd.pipeline.name": "JOB",
                    "cicd.provider.name": "github_actions",
                    "vcs.ref.head.revision": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
                    "service.name": "unknown_service",
                    "telemetry.sdk.language": "python",
                    "telemetry.sdk.name": "opentelemetry",
                    "telemetry.sdk.version": anys.ANY_STR,
                },
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
                "test.case.result.status": "failure",
                "test.type": "case",
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
                "attributes": {
                    "cicd.pipeline.name": "JOB",
                    "cicd.provider.name": "github_actions",
                    "vcs.ref.head.revision": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
                    "service.name": "unknown_service",
                    "telemetry.sdk.language": "python",
                    "telemetry.sdk.name": "opentelemetry",
                    "telemetry.sdk.version": anys.ANY_STR,
                },
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
                "test.type": "suite",
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
                "attributes": {
                    "cicd.pipeline.name": "JOB",
                    "cicd.provider.name": "github_actions",
                    "vcs.ref.head.revision": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
                    "service.name": "unknown_service",
                    "telemetry.sdk.language": "python",
                    "telemetry.sdk.name": "opentelemetry",
                    "telemetry.sdk.version": anys.ANY_STR,
                },
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
                "test.case.result.status": "success",
                "test.type": "case",
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
                "attributes": {
                    "cicd.pipeline.name": "JOB",
                    "cicd.provider.name": "github_actions",
                    "vcs.ref.head.revision": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
                    "service.name": "unknown_service",
                    "telemetry.sdk.language": "python",
                    "telemetry.sdk.name": "opentelemetry",
                    "telemetry.sdk.version": anys.ANY_STR,
                },
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
                "test.case.result.status": "failure",
                "test.type": "case",
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
                "attributes": {
                    "service.name": "unknown_service",
                    "telemetry.sdk.language": "python",
                    "telemetry.sdk.name": "opentelemetry",
                    "telemetry.sdk.version": anys.ANY_STR,
                    "cicd.pipeline.name": "JOB",
                    "cicd.provider.name": "github_actions",
                    "vcs.ref.head.revision": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
                },
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
                "test.case.result.status": "success",
                "test.type": "case",
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
                "attributes": {
                    "cicd.pipeline.name": "JOB",
                    "cicd.provider.name": "github_actions",
                    "vcs.ref.head.revision": "3af96aa24f1d32fcfbb7067793cacc6dc0c6b199",
                    "service.name": "unknown_service",
                    "telemetry.sdk.language": "python",
                    "telemetry.sdk.name": "opentelemetry",
                    "telemetry.sdk.version": anys.ANY_STR,
                },
                "schema_url": "",
            },
            "start_time": anys.ANY_DATETIME_STR,
            "status": {
                "status_code": "OK",
            },
        },
    ]
