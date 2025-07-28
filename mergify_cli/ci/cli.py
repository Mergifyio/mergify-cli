import sys

import click
import httpx
import opentelemetry.trace
import tenacity

from mergify_cli import utils
from mergify_cli.ci import detector
from mergify_cli.ci import junit
from mergify_cli.ci import upload


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
    envvar=["GITHUB_BASE_REF", "MERGIFY_TESTS_TARGET_BRANCH"],
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
    envvar=["GITHUB_BASE_REF", "MERGIFY_TESTS_TARGET_BRANCH"],
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

    failing_spans = [
        span
        for span in spans
        if span.status.status_code == opentelemetry.trace.StatusCode.ERROR
        and span.attributes is not None
        and span.attributes.get("test.scope") == "case"
    ]
    if not failing_spans:
        return

    await check_failing_spans_with_quarantine(
        api_url,
        token,
        repository,
        tests_target_branch,
        [fspan.name for fspan in failing_spans],
    )


@tenacity.retry(
    wait=tenacity.wait_exponential(multiplier=0.2),
    stop=tenacity.stop_after_attempt(5),
    retry=tenacity.retry_if_exception_type(httpx.TransportError),
    reraise=True,
)
async def check_failing_spans_with_quarantine(
    api_url: str,
    token: str,
    repository: str,
    tests_target_branch: str,
    failing_spans_names: list[str],
) -> None:
    fspans_str = "\n".join(failing_spans_names)
    click.echo(
        f"Checking the following failing tests for quarantine:\n{fspans_str}",
        err=False,
    )

    try:
        repo_owner, repo_name = repository.split("/")
    except ValueError:
        click.echo(
            click.style(
                f"Unable to extract repository owner and name from {repository}",
                fg="red",
            ),
            err=True,
        )
        sys.exit(1)

    async with utils.get_http_client(
        server=f"{api_url}/v1/ci/{repo_owner}/repositories/{repo_name}/quarantines",
        headers={"Authorization": f"Bearer {token}"},
    ) as client:
        response = await client.post(
            "/check",
            json={"tests_names": failing_spans_names, "branch": tests_target_branch},
        )

        if response.status_code != 200:
            click.echo(
                click.style(
                    f"HTTP error {response.status_code} while checking quarantined tests: {response.text}",
                    fg="red",
                ),
                err=True,
            )
            sys.exit(1)

        resp_json = response.json()
        if resp_json["quarantined_tests_names"]:
            quarantined_test_names_str = "\n".join(
                resp_json["quarantined_tests_names"],
            )
            click.echo(
                f"The following failing tests are quarantined and will be ignored:\n{quarantined_test_names_str}",
                err=False,
            )

        if not resp_json["non_quarantined_tests_names"]:
            return

        non_quarantined_test_names_str = "\n".join(
            resp_json["non_quarantined_tests_names"],
        )
        click.echo(
            click.style(
                f"The following failing tests are not quarantined:\n{non_quarantined_test_names_str}",
                fg="red",
            ),
            err=True,
        )
        sys.exit(1)
