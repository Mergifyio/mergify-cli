import dataclasses
import os
import sys
import typing

import click
import httpx
from opentelemetry.sdk.trace import ReadableSpan
import opentelemetry.trace
import tenacity

from mergify_cli import utils
from mergify_cli.ci import detector
from mergify_cli.ci import junit
from mergify_cli.ci import upload


def _process_tests_target_branch(
    _ctx: click.Context,
    _param: click.Parameter,
    value: str | None,
) -> str | None:
    """Process the tests_target_branch parameter to strip refs/heads/ prefix from GITHUB_REF."""
    return value.removeprefix("refs/heads/") if value else value


ci = click.Group(
    "ci",
    help="Mergify's CI related commands",
)


@ci.command(help="Upload JUnit XML reports", deprecated="Use `junit-process` instead")
@click.option(
    "--api-url",
    "-u",
    help="URL of the Mergify API",
    required=True,
    envvar="MERGIFY_API_URL",
    default="https://api.mergify.com",
    show_default=True,
)
@click.option(
    "--token",
    "-t",
    help="CI Issues Application Key",
    required=True,
    envvar="MERGIFY_TOKEN",
)
@click.option(
    "--repository",
    "-r",
    help="Repository full name (owner/repo)",
    required=True,
    default=detector.get_github_repository,
)
@click.option(
    "--test-framework",
    help="Test framework",
)
@click.option(
    "--test-language",
    help="Test language",
)
@click.option(
    "--tests-target-branch",
    "-ttb",
    help="The branch used to check if failing tests can be ignored with Mergify's Quarantine.",
    required=True,
    envvar=["GITHUB_BASE_REF", "GITHUB_REF_NAME", "GITHUB_REF"],
    callback=_process_tests_target_branch,
)
@click.argument(
    "files",
    nargs=-1,
    required=True,
    type=click.Path(exists=True, dir_okay=False),
)
@utils.run_with_asyncio
async def junit_upload(  # noqa: PLR0913
    *,
    api_url: str,
    token: str,
    repository: str,
    test_framework: str | None,
    test_language: str | None,
    tests_target_branch: str,
    files: tuple[str, ...],
) -> None:
    await _process_junit_files(
        api_url=api_url,
        token=token,
        repository=repository,
        test_framework=test_framework,
        test_language=test_language,
        tests_target_branch=tests_target_branch,
        files=files,
    )


@ci.command(
    help="""Upload JUnit XML reports and ignore failed tests with Mergify's CI Insights Quarantine""",
    short_help="""Upload JUnit XML reports and ignore failed tests with Mergify's CI Insights Quarantine""",
)
@click.option(
    "--api-url",
    "-u",
    help="URL of the Mergify API",
    required=True,
    envvar="MERGIFY_API_URL",
    default="https://api.mergify.com",
    show_default=True,
)
@click.option(
    "--token",
    "-t",
    help="CI Issues Application Key",
    required=True,
    envvar="MERGIFY_TOKEN",
)
@click.option(
    "--repository",
    "-r",
    help="Repository full name (owner/repo)",
    required=True,
    default=detector.get_github_repository,
)
@click.option(
    "--test-framework",
    help="Test framework",
)
@click.option(
    "--test-language",
    help="Test language",
)
@click.option(
    "--tests-target-branch",
    "-ttb",
    help="The branch used to check if failing tests can be ignored with Mergify's Quarantine.",
    required=True,
    envvar=["GITHUB_BASE_REF", "GITHUB_REF_NAME", "GITHUB_REF"],
    callback=_process_tests_target_branch,
)
@click.argument(
    "files",
    nargs=-1,
    required=True,
    type=click.Path(exists=True, dir_okay=False),
)
@utils.run_with_asyncio
async def junit_process(  # noqa: PLR0913
    *,
    api_url: str,
    token: str,
    repository: str,
    test_framework: str | None,
    test_language: str | None,
    tests_target_branch: str,
    files: tuple[str, ...],
) -> None:
    await _process_junit_files(
        api_url=api_url,
        token=token,
        repository=repository,
        test_framework=test_framework,
        test_language=test_language,
        tests_target_branch=tests_target_branch,
        files=files,
    )


@dataclasses.dataclass
class QuarantineFailedError(Exception):
    message: str


QUARANTINE_INFO_ERROR_MSG = (
    "This error occurred because there are failed tests in your CI pipeline and will disappear once your CI passes successfully.\n\n"
    "If you're unsure why this is happening or need assistance, please contact Mergify to report the issue."
)


async def _process_junit_files(  # noqa: PLR0913
    *,
    api_url: str,
    token: str,
    repository: str,
    test_framework: str | None,
    test_language: str | None,
    tests_target_branch: str,
    files: tuple[str, ...],
) -> None:
    try:
        spans = await junit.files_to_spans(
            files,
            test_language=test_language,
            test_framework=test_framework,
        )
    except junit.InvalidJunitXMLError as e:
        click.echo(
            click.style(
                f"Error converting JUnit XML file to spans: {e.details}",
                fg="red",
            ),
            err=True,
        )
        sys.exit(1)

    if not spans:
        click.echo(
            click.style("No spans found in the JUnit files", fg="red"),
            err=True,
        )
        sys.exit(1)

    # NOTE: Check quarantine before uploading in order to properly modify the
    # "cicd.test.quarantined" attribute for the required spans.
    try:
        failing_tests_not_quarantined_count = await check_failing_spans_with_quarantine(
            api_url,
            token,
            repository,
            tests_target_branch,
            spans,
        )
    except QuarantineFailedError as exc:
        click.echo(click.style(exc.message, fg="red"), err=True)
        click.echo(click.style(QUARANTINE_INFO_ERROR_MSG, fg="red"), err=True)
        quarantine_exit_error_code = 1
    except Exception as exc:  # noqa: BLE001
        msg = f"An unexpected error occured when checking quarantined tests: {exc!s}"
        click.echo(click.style(msg, fg="red"), err=True)
        click.echo(click.style(QUARANTINE_INFO_ERROR_MSG, fg="red"), err=True)
        quarantine_exit_error_code = 1
    else:
        quarantine_exit_error_code = 1 if failing_tests_not_quarantined_count > 0 else 0

    try:
        upload.upload(
            api_url=api_url,
            token=token,
            repository=repository,
            spans=spans,
        )
    except Exception as e:  # noqa: BLE001
        click.echo(
            click.style(f"Error uploading JUnit XML reports: {e}", fg="red"),
            err=True,
        )

    if quarantine_exit_error_code != 0:
        sys.exit(quarantine_exit_error_code)


async def check_failing_spans_with_quarantine(
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

    failing_spans = [
        span
        for span in spans
        if span.status.status_code == opentelemetry.trace.StatusCode.ERROR
        and span.attributes is not None
        and span.attributes.get("test.scope") == "case"
    ]
    failing_spans_name = [fspan.name for fspan in failing_spans]
    if not failing_spans:
        return 0

    failing_tests_not_quarantined_count: int = 0
    quarantined_tests_tuple = await _check_failing_spans_with_quarantine(
        api_url,
        token,
        repository,
        tests_target_branch,
        failing_spans_name,
    )

    if quarantined_tests_tuple.quarantined_tests_names:
        quarantined_test_names_str = os.linesep.join(
            quarantined_tests_tuple.quarantined_tests_names,
        )
        click.echo(
            f"The following failing tests are quarantined and will be ignored:{os.linesep}{quarantined_test_names_str}",
            err=False,
        )

    if quarantined_tests_tuple.non_quarantined_tests_names:
        non_quarantined_test_names_str = os.linesep.join(
            quarantined_tests_tuple.non_quarantined_tests_names,
        )
        click.echo(
            click.style(
                f"{os.linesep}The following failing tests are not quarantined:{os.linesep}{non_quarantined_test_names_str}",
                fg="red",
            ),
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
async def _check_failing_spans_with_quarantine(
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

    fspans_str = os.linesep.join(failing_spans_names)
    click.echo(
        f"Checking the following failing tests for quarantine:{os.linesep}{fspans_str}",
        err=False,
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
