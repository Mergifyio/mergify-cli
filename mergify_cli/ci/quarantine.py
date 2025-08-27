import dataclasses
import os
import typing

import click
import httpx
from opentelemetry.sdk.trace import ReadableSpan
import opentelemetry.trace
import tenacity

from mergify_cli import utils


@dataclasses.dataclass
class QuarantineFailedError(Exception):
    message: str


QUARANTINE_INFO_ERROR_MSG = (
    "This error occurred because there are failed tests in your CI pipeline and will disappear once your CI passes successfully.\n\n"
    "If you're unsure why this is happening or need assistance, please contact Mergify to report the issue."
)


async def check_and_update_failing_spans(
    api_url: str,
    token: str,
    repository: str,
    tests_target_branch: str,
    spans: list[ReadableSpan],
) -> int:
    """
    Check all the `spans` with CI Insights Quarantine by:
        - logging the failed and quarantined test
        - logging the failed and non-quarantined test as error message
        - updating the `spans` of quarantined tests by setting the attribute `cicd.test.quarantined` to `true`

    Returns the number of failing tests that are not quarantined.
    """

    click.echo(f"{os.linesep}🛡️ Quarantine")

    failing_spans = [
        span
        for span in spans
        if span.status.status_code == opentelemetry.trace.StatusCode.ERROR
        and span.attributes is not None
        and span.attributes.get("test.scope") == "case"
    ]

    failing_spans_name = [fspan.name for fspan in failing_spans]
    if not failing_spans:
        click.echo(
            "• No quarantine check required since no failed tests were detected",
        )
        return 0

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

        span._attributes = dict(span.attributes) | {  # noqa: SLF001
            "cicd.test.quarantined": quarantined,
        }
        if (
            not quarantined
            and span.status.status_code == opentelemetry.trace.StatusCode.ERROR
        ):
            failing_tests_not_quarantined_count += 1

    quarantined_tests_spans = [
        span
        for span in failing_spans
        if span.name in quarantined_tests_tuple.quarantined_tests_names
    ]

    non_quarantined_tests_spans = [
        span
        for span in failing_spans
        if span.name in quarantined_tests_tuple.non_quarantined_tests_names
    ]

    click.echo(
        f"• Quarantined failures matched: {len(quarantined_tests_spans)}/{len(failing_spans)}",
    )
    if quarantined_tests_spans:
        click.echo("  - 🔒 Quarantined:")
        for qt_span in quarantined_tests_spans:
            click.echo(f"      · {qt_span.name}")

    if non_quarantined_tests_spans:
        click.echo("  - ❌ Unquarantined:")
        for nqt_span in non_quarantined_tests_spans:
            click.echo(f"      · {nqt_span.name}")

    return failing_tests_not_quarantined_count


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
                message=f"HTTP error {response.status_code} while checking quarantined tests: {response.text}",
            )

        return QuarantinedTests(
            quarantined_tests_names=response.json()["quarantined_tests_names"],
            non_quarantined_tests_names=response.json()["non_quarantined_tests_names"],
        )
