from __future__ import annotations

import glob
import pathlib

import click

from mergify_cli import utils
from mergify_cli.ci import detector
from mergify_cli.ci.git_refs import detector as git_refs_detector
from mergify_cli.ci.junit_processing import cli as junit_processing_cli
from mergify_cli.ci.scopes import cli as scopes_cli
from mergify_cli.ci.scopes import exceptions as scopes_exc
from mergify_cli.dym import DYMGroup
from mergify_cli.exit_codes import ExitCode


def _expand_junit_patterns(
    ctx: click.Context,
    param: click.Parameter,
    value: tuple[str, ...],
) -> tuple[str, ...]:
    # Accept raw glob patterns and expand them here so callers don't have to
    # rely on shell expansion — preferable for large test suites.
    results: dict[str, None] = {}
    for entry in value:
        literal = pathlib.Path(entry)
        # Existing literal paths take precedence so filenames that happen to
        # contain glob metacharacters (e.g. `report[1].xml`) keep working.
        if literal.is_file():
            results.setdefault(entry, None)
            continue

        if glob.has_magic(entry):
            matches = [
                match
                for match in glob.iglob(entry, recursive=True)  # noqa: PTH207
                if pathlib.Path(match).is_file()
            ]
            if not matches:
                raise click.BadParameter(
                    f"Pattern '{entry}' did not match any file.\n\n"
                    "This usually indicates that a previous CI step failed to generate the test results.\n"
                    "Please check if your test execution step completed successfully and produced the expected output files.",
                    ctx=ctx,
                    param=param,
                )

            results.update(dict.fromkeys(matches))
            continue

        if literal.is_dir():
            raise click.BadParameter(
                f"'{entry}' is a directory, not a JUnit XML file.\n\n"
                "Pass a file path or a quoted glob pattern (e.g. 'reports/**/*.xml') instead.",
                ctx=ctx,
                param=param,
            )

        raise click.BadParameter(
            f"JUnit XML file '{entry}' does not exist.\n\n"
            "This usually indicates that a previous CI step failed to generate the test results.\n"
            "Please check if your test execution step completed successfully and produced the expected output file.",
            ctx=ctx,
            param=param,
        )

    return tuple(results)


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
    default=detector.get_tests_target_branch,
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
    callback=_expand_junit_patterns,
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
    help=(
        "Upload JUnit XML reports and ignore failed tests with Mergify's CI"
        " Insights Quarantine.\n\nFILES can be literal paths or quoted glob"
        " patterns (e.g. 'reports/**/*.xml'); quoting lets Mergify expand the"
        " pattern rather than the shell, which is recommended for large test"
        " suites."
    ),
    short_help=(
        "Upload JUnit XML reports and ignore failed tests with Mergify's CI"
        " Insights Quarantine"
    ),
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
    default=detector.get_tests_target_branch,
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
    callback=_expand_junit_patterns,
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
        raise utils.MergifyError(msg, exit_code=ExitCode.CONFIGURATION_ERROR)

    if not pathlib.Path(config_path).is_file():
        msg = f"Config file '{config_path}' does not exist."
        raise utils.MergifyError(msg, exit_code=ExitCode.CONFIGURATION_ERROR)

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
        raise utils.MergifyError(
            str(e),
            exit_code=ExitCode.CONFIGURATION_ERROR,
        ) from e

    if write is not None:
        scopes.save_to_file(write)
