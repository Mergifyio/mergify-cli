import sys

import click

from mergify_cli import utils
from mergify_cli.ci import detector
from mergify_cli.ci import junit
from mergify_cli.ci import quarantine
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
        failing_tests_not_quarantined_count = (
            await quarantine.check_and_update_failing_spans(
                api_url,
                token,
                repository,
                tests_target_branch,
                spans,
            )
        )
    except quarantine.QuarantineFailedError as exc:
        click.echo(click.style(exc.message, fg="red"), err=True)
        click.echo(
            click.style(quarantine.QUARANTINE_INFO_ERROR_MSG, fg="red"),
            err=True,
        )
        quarantine_exit_error_code = 1
    except Exception as exc:  # noqa: BLE001
        msg = f"An unexpected error occured when checking quarantined tests: {exc!s}"
        click.echo(click.style(msg, fg="red"), err=True)
        click.echo(
            click.style(quarantine.QUARANTINE_INFO_ERROR_MSG, fg="red"),
            err=True,
        )
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
