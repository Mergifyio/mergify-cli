from __future__ import annotations

import dataclasses
import typing

import httpx
import opentelemetry.trace
import tenacity

from mergify_cli import utils


if typing.TYPE_CHECKING:
    from opentelemetry.sdk.trace import ReadableSpan


@dataclasses.dataclass
class QuarantineFailedError(Exception):
    message: str


@dataclasses.dataclass(frozen=True)
class QuarantineResult:
    failing_spans: list[ReadableSpan]
    quarantined_spans: list[ReadableSpan]
    non_quarantined_spans: list[ReadableSpan]
    failing_tests_not_quarantined_count: int


async def check_and_update_failing_spans(
    api_url: str,
    token: str,
    repository: str,
    tests_target_branch: str,
    spans: list[ReadableSpan],
) -> QuarantineResult:
    """
    Check all the `spans` with CI Insights Quarantine by:
        - updating the `spans` of quarantined tests by setting the attribute `cicd.test.quarantined` to `true`

    Returns a QuarantineResult with the categorized spans.
    """

    failing_spans = [
        span
        for span in spans
        if span.status.status_code == opentelemetry.trace.StatusCode.ERROR
        and span.attributes is not None
        and span.attributes.get("test.scope") == "case"
    ]

    if not failing_spans:
        return QuarantineResult(
            failing_spans=[],
            quarantined_spans=[],
            non_quarantined_spans=[],
            failing_tests_not_quarantined_count=0,
        )

    failing_spans_name = [fspan.name for fspan in failing_spans]
    failing_tests_not_quarantined_count: int = 0
    quarantined_tests_tuple = await fetch_quarantined_tests_from_failing_spans(
        api_url,
        token,
        repository,
        tests_target_branch,
        failing_spans_name,
    )

    for span in spans:
        if span.attributes is None or span.attributes.get("test.scope") != "case":
            continue

        quarantined = bool(
            span.name in quarantined_tests_tuple.quarantined_tests_names,
        )

        span._attributes = dict(span.attributes) | {
            "cicd.test.quarantined": quarantined,
        }
        if (
            not quarantined
            and span.status.status_code == opentelemetry.trace.StatusCode.ERROR
        ):
            failing_tests_not_quarantined_count += 1

    quarantined_spans = [
        span
        for span in failing_spans
        if span.name in quarantined_tests_tuple.quarantined_tests_names
    ]

    non_quarantined_spans = [
        span
        for span in failing_spans
        if span.name in quarantined_tests_tuple.non_quarantined_tests_names
    ]

    return QuarantineResult(
        failing_spans=failing_spans,
        quarantined_spans=quarantined_spans,
        non_quarantined_spans=non_quarantined_spans,
        failing_tests_not_quarantined_count=failing_tests_not_quarantined_count,
    )


class QuarantinedTests(typing.NamedTuple):
    quarantined_tests_names: list[str]
    non_quarantined_tests_names: list[str]


@tenacity.retry(
    wait=tenacity.wait_exponential(multiplier=0.2),
    stop=tenacity.stop_after_attempt(5),
    retry=tenacity.retry_if_exception_type(httpx.TransportError),
    reraise=True,
)
async def fetch_quarantined_tests_from_failing_spans(
    api_url: str,
    token: str,
    repository: str,
    tests_target_branch: str,
    failing_spans_names: list[str],
) -> QuarantinedTests:
    try:
        repo_owner, repo_name = repository.split("/")
    except ValueError:
        raise QuarantineFailedError(
            message=f"Unable to extract repository owner and name from {repository}",
        )

    async with utils.get_http_client(
        server=f"{api_url}/v1/ci/{repo_owner}/repositories/{repo_name}/quarantines",
        headers={"Authorization": f"Bearer {token}"},
    ) as client:
        response = await client.post(
            "/check",
            json={"tests_names": failing_spans_names, "branch": tests_target_branch},
        )

        if response.status_code != 200:
            raise QuarantineFailedError(
                message=f"HTTP {response.status_code}: {response.text}",
            )

        return QuarantinedTests(
            quarantined_tests_names=response.json()["quarantined_tests_names"],
            non_quarantined_tests_names=response.json()["non_quarantined_tests_names"],
        )
