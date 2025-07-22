import sys

import click
import opentelemetry.trace

from mergify_cli import utils
from mergify_cli.ci import detector
from mergify_cli.ci import upload


ci = click.Group(
    "ci",
    help="Mergify's CI related commands",
)


@ci.command(help="Upload JUnit XML reports")
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
    "--quarantine-check",
    help="""If this flag is specified, the error code returned by the cli will be `1` if any of the failing tests in the junit file are not quarantined.
If all the failing tests in the junit file are either a success or quanratined, then the exit code will be `0`.
By default this flag is not set.
""",
    is_flag=True,
    default=False,
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
    quarantine_check: bool,
    files: tuple[str, ...],
) -> None:
    try:
        spans = await upload.upload(
            api_url=api_url,
            token=token,
            repository=repository,
            test_framework=test_framework,
            test_language=test_language,
            files=files,
        )
    except Exception as e:  # noqa: BLE001
        spans = []
        click.echo(
            click.style(f"Error uploading JUnit XML reports: {e}", fg="red"),
            err=True,
        )

    if spans and quarantine_check:
        failing_spans = [
            span
            for span in spans
            if span.status.status_code == opentelemetry.trace.StatusCode.ERROR
            and span.attributes is not None
            and span.attributes.get("test.scope") == "case"
        ]
        if not failing_spans:
            return

        await check_failing_spans(
            api_url,
            token,
            repository,
            [fspan.name for fspan in failing_spans],
        )


async def check_failing_spans(
    api_url: str,
    token: str,
    repository: str,
    failing_spans_names: list[str],
) -> None:
    fspans_str = "\n".join(failing_spans_names)
    click.echo(
        f"Checking if the following failing tests for quarantine:\n{fspans_str}",
        err=False,
    )

    async with utils.get_http_client(
        server=f"{api_url}/v1/ci/{repository}/quarantine",
        headers={"Authorization": f"Bearer {token}"},
    ) as client:
        response = await client.post(
            "/check",
            json={"tests_names": failing_spans_names},
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
