from __future__ import annotations

import json
import os
import pathlib
import shlex
import uuid

import click

from mergify_cli import utils
from mergify_cli.ci import detector
from mergify_cli.ci.git_refs import detector as git_refs_detector
from mergify_cli.ci.junit_processing import cli as junit_processing_cli
from mergify_cli.ci.queue import metadata as queue_metadata
from mergify_cli.ci.scopes import cli as scopes_cli
from mergify_cli.ci.scopes import exceptions as scopes_exc
from mergify_cli.dym import DYMGroup


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


@click.group(
    cls=DYMGroup,
    invoke_without_command=True,
    help="Mergify's CI related commands",
)
@click.pass_context
def ci(ctx: click.Context) -> None:
    if ctx.invoked_subcommand is None:
        click.echo(ctx.get_help())


@ci.command(help="Upload JUnit XML reports", deprecated="Use `junit-process` instead")
@click.option(
    "--api-url",
    "-u",
    help="URL of the Mergify API",
    required=True,
    envvar="MERGIFY_API_URL",
    default=utils.MERGIFY_API_DEFAULT_URL,
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
    envvar=["GITHUB_BASE_REF", "GITHUB_HEAD_REF", "GITHUB_REF_NAME", "GITHUB_REF"],
    callback=_process_tests_target_branch,
)
@click.option(
    "--test-exit-code",
    "-e",
    help="Exit code of the test runner process. Used to detect silent failures where the runner crashed but the JUnit report appears clean.",
    type=int,
    required=False,
    default=None,
    envvar="MERGIFY_TEST_EXIT_CODE",
)
@click.argument(
    "files",
    nargs=-1,
    required=True,
    type=JUnitFile(),
)
@utils.run_with_asyncio
async def junit_upload(
    *,
    api_url: str,
    token: str,
    repository: str,
    test_framework: str | None,
    test_language: str | None,
    tests_target_branch: str,
    test_exit_code: int | None,
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
        test_exit_code=test_exit_code,
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
    default=utils.MERGIFY_API_DEFAULT_URL,
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
    envvar=["GITHUB_BASE_REF", "GITHUB_HEAD_REF", "GITHUB_REF_NAME", "GITHUB_REF"],
    callback=_process_tests_target_branch,
)
@click.option(
    "--test-exit-code",
    "-e",
    help="Exit code of the test runner process. Used to detect silent failures where the runner crashed but the JUnit report appears clean.",
    type=int,
    required=False,
    default=None,
    envvar="MERGIFY_TEST_EXIT_CODE",
)
@click.argument(
    "files",
    nargs=-1,
    required=True,
    type=JUnitFile(),
)
@utils.run_with_asyncio
async def junit_process(
    *,
    api_url: str,
    token: str,
    repository: str,
    test_framework: str | None,
    test_language: str | None,
    tests_target_branch: str,
    test_exit_code: int | None,
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
        test_exit_code=test_exit_code,
    )


@ci.command(
    help="""Give the base/head git references of the pull request""",
    short_help="""Give the base/head git references of the pull request""",
)
@click.option(
    "--format",
    "output_format",
    type=click.Choice(["text", "shell", "json"]),
    default="text",
    show_default=True,
    help=(
        "Output format. 'text' is human-readable. "
        "'shell' emits MERGIFY_GIT_REFS_{BASE,HEAD,SOURCE}=... lines for `eval`. "
        "'json' emits a single-line JSON object."
    ),
)
def git_refs(output_format: str) -> None:
    ref = git_refs_detector.detect()

    if output_format == "shell":
        click.echo(f"MERGIFY_GIT_REFS_BASE={shlex.quote(ref.base or '')}")
        click.echo(f"MERGIFY_GIT_REFS_HEAD={shlex.quote(ref.head)}")
        click.echo(f"MERGIFY_GIT_REFS_SOURCE={shlex.quote(ref.source)}")
    elif output_format == "json":
        click.echo(
            json.dumps({"base": ref.base, "head": ref.head, "source": ref.source}),
        )
    else:
        click.echo(f"Base: {ref.base}")
        click.echo(f"Head: {ref.head}")

    ref.maybe_write_to_github_outputs()


@ci.command(
    help="""Give the list scope impacted by changed files""",
    short_help="""Give the list scope impacted by changed files""",
)
@click.option(
    "--config",
    "config_path",
    type=click.Path(dir_okay=False),
    envvar="MERGIFY_CONFIG_PATH",
    default=detector.get_mergify_config_path,
    help="Path to YAML config file.",
)
@click.option(
    "--base",
    help="The base git reference to use to look for changed files",
)
@click.option(
    "--head",
    help="The head git reference to use to look for changed files",
)
@click.option(
    "--write",
    "-w",
    type=click.Path(),
    help="Write the detected scopes to a file (json).",
)
def scopes(
    config_path: str | None,
    write: str | None = None,
    head: str | None = None,
    base: str | None = None,
) -> None:
    # Empty envvar (MERGIFY_CONFIG_PATH="") should fall back to autodetect
    if config_path is not None and not config_path:
        config_path = detector.get_mergify_config_path()

    if config_path is None:
        locations = ", ".join(detector.MERGIFY_CONFIG_PATHS)
        msg = f"Mergify configuration file not found. Looked in: {locations}"
        raise click.ClickException(msg)

    if not pathlib.Path(config_path).is_file():
        msg = f"Config file '{config_path}' does not exist."
        raise click.ClickException(msg)

    if base or head:
        ref = git_refs_detector.References(
            base=base,
            head=head or "HEAD",
            source="manual",
        )
    else:
        ref = git_refs_detector.detect()

    try:
        scopes = scopes_cli.detect(
            config_path=config_path,
            references=ref,
        )
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
    default=utils.MERGIFY_API_DEFAULT_URL,
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
)
@click.option("--scope", "-s", multiple=True, help="Scope to upload")
@click.option(
    "--file",
    "-f",
    help="File containing scopes to upload",
    type=click.Path(exists=True),
)
@utils.run_with_asyncio
async def scopes_send(
    api_url: str,
    token: str,
    repository: str,
    pull_request: int | None,
    scope: tuple[str, ...],
    file: str | None,
) -> None:
    if pull_request is None:
        click.echo("No pull request number detected, skipping scopes upload.")
        return

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


@ci.command(
    help="""Output merge queue batch metadata from the current pull request event""",
    short_help="""Output merge queue batch metadata""",
)
def queue_info() -> None:
    metadata = queue_metadata.detect()
    if metadata is None:
        raise click.ClickException(
            "Not running in a merge queue context. "
            "This command must be run on a merge queue draft pull request.",
        )

    click.echo(json.dumps(metadata, indent=2))

    gha = os.environ.get("GITHUB_OUTPUT")
    if gha:
        delimiter = f"ghadelimiter_{uuid.uuid4()}"
        with pathlib.Path(gha).open("a", encoding="utf-8") as fh:
            fh.write(
                f"queue_metadata<<{delimiter}\n{json.dumps(metadata)}\n{delimiter}\n",
            )
