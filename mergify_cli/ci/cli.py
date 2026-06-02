from __future__ import annotations

import pathlib

import click

from mergify_cli import utils
from mergify_cli.ci import detector
from mergify_cli.ci.git_refs import detector as git_refs_detector
from mergify_cli.ci.scopes import cli as scopes_cli
from mergify_cli.ci.scopes import exceptions as scopes_exc
from mergify_cli.dym import DYMGroup
from mergify_cli.exit_codes import ExitCode


@click.group(
    cls=DYMGroup,
    invoke_without_command=True,
    help="Mergify's CI related commands",
)
@click.pass_context
def ci(ctx: click.Context) -> None:
    if ctx.invoked_subcommand is None:
        click.echo(ctx.get_help())


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
