import click

from mergify_cli import utils
from mergify_cli.ci import detector
from mergify_cli.ci.junit_processing import cli as junit_processing_cli
from mergify_cli.ci.scopes import cli as scopes_cli
from mergify_cli.ci.scopes import exceptions as scopes_exc


class JUnitFile(click.Path):
    """Custom Click parameter type for JUnit files with better error messages."""

    def __init__(self) -> None:
        super().__init__(exists=True, dir_okay=False)

    def convert(  # type: ignore[override]
        self,
        value: str,
        param: click.Parameter | None,
        ctx: click.Context | None,
    ) -> str:
        try:
            return super().convert(value, param, ctx)
        except click.BadParameter as e:
            if "does not exist" in str(e):
                # Provide a more helpful error message
                error_msg = (
                    f"JUnit XML file '{value}' does not exist. \n\n"
                    "This usually indicates that a previous CI step failed to generate the test results.\n"
                    "Please check if your test execution step completed successfully and produced the expected output file."
                )
                raise click.BadParameter(
                    error_msg,
                    ctx=ctx,
                    param=param,
                ) from e
            raise


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
    type=JUnitFile(),
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
    await junit_processing_cli.process_junit_files(
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
    type=JUnitFile(),
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
    await junit_processing_cli.process_junit_files(
        api_url=api_url,
        token=token,
        repository=repository,
        test_framework=test_framework,
        test_language=test_language,
        tests_target_branch=tests_target_branch,
        files=files,
    )


@ci.command(
    help="""Give the list scope impacted by changed files""",
    short_help="""Give the list scope impacted by changed files""",
)
@click.option(
    "--config",
    "config_path",
    required=True,
    type=click.Path(exists=True),
    default=".mergify.yml",
    help="Path to YAML config file.",
)
@click.option(
    "--write",
    "-w",
    type=click.Path(),
    help="Write the detected scopes to a file (json).",
)
def scopes(
    config_path: str,
    write: str | None = None,
) -> None:
    try:
        scopes = scopes_cli.detect(config_path=config_path)
    except scopes_exc.ScopesError as e:
        raise click.ClickException(str(e)) from e

    if write is not None:
        scopes.save_to_file(write)


@ci.command(help="Send scopes tied to a pull request to Mergify")
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
    help="Mergify Key",
    envvar="MERGIFY_TOKEN",
    required=True,
)
@click.option(
    "--repository",
    "-r",
    help="Repository full name (owner/repo)",
    default=detector.get_github_repository,
    required=True,
)
@click.option(
    "--pull-request",
    "-p",
    help="pull_request number",
    type=int,
    default=detector.get_github_pull_request_number,
    required=True,
)
@click.option("--scope", "-s", multiple=True, help="Scope to upload")
@click.option(
    "--file",
    "-f",
    help="File containing scopes to upload",
    type=click.Path(exists=True),
)
@utils.run_with_asyncio
async def scopes_send(  # noqa: PLR0913, PLR0917
    api_url: str,
    token: str,
    repository: str,
    pull_request: int,
    scope: tuple[str, ...],
    file: str | None,
) -> None:
    scopes = list(scope)
    if file is not None:
        try:
            dump = scopes_cli.DetectedScope.load_from_file(file)
        except scopes_exc.ScopesError as e:
            raise click.ClickException(str(e)) from e
        scopes.extend(dump.scopes)

    await scopes_cli.send_scopes(
        api_url,
        token,
        repository,
        pull_request,
        scopes,
    )
