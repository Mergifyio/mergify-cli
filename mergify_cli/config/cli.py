from __future__ import annotations

import asyncio
import pathlib
import re

import click
import httpx
from rich.markdown import Markdown
from rich.markup import escape
import yaml

from mergify_cli import console
from mergify_cli import console_error
from mergify_cli import utils
from mergify_cli.ci.detector import MERGIFY_CONFIG_PATHS
from mergify_cli.ci.detector import get_mergify_config_path
from mergify_cli.config import validate as config_validate
from mergify_cli.dym import DYMGroup
from mergify_cli.exit_codes import ExitCode


def _resolve_config_path(config_path: str | None) -> str:
    if config_path is None:
        config_path = get_mergify_config_path()

    if config_path is None:
        locations = ", ".join(MERGIFY_CONFIG_PATHS)
        msg = f"Mergify configuration file not found. Looked in: {locations}"
        raise utils.MergifyError(msg, exit_code=ExitCode.CONFIGURATION_ERROR)

    if not pathlib.Path(config_path).is_file():
        msg = f"Configuration file not found: {config_path}"
        raise utils.MergifyError(msg, exit_code=ExitCode.CONFIGURATION_ERROR)

    return config_path


@click.group(
    cls=DYMGroup,
    invoke_without_command=True,
    help="Manage Mergify configuration",
)
@click.option(
    "--config-file",
    "-f",
    type=click.Path(dir_okay=False),
    default=None,
    help="Path to the Mergify configuration file (auto-detected if not provided)",
)
@click.pass_context
def config(ctx: click.Context, *, config_file: str | None) -> None:
    ctx.ensure_object(dict)
    ctx.obj["config_file"] = config_file
    if ctx.invoked_subcommand is None:
        click.echo(ctx.get_help())


@config.command(help="Validate the Mergify configuration file against the schema")
@click.pass_context
def validate(ctx: click.Context) -> None:
    config_path = _resolve_config_path(ctx.obj["config_file"])

    try:
        config_data = config_validate.load_yaml(config_path)
    except yaml.YAMLError as e:
        raise utils.MergifyError(
            f"Invalid YAML in {config_path}: {e}",
            exit_code=ExitCode.CONFIGURATION_ERROR,
        ) from e
    except (TypeError, OSError) as e:
        raise utils.MergifyError(
            str(e),
            exit_code=ExitCode.CONFIGURATION_ERROR,
        ) from e

    try:
        with httpx.Client(timeout=30) as client:
            schema = config_validate.fetch_schema(client)
        result = config_validate.validate_config(config_data, schema)
    except httpx.HTTPError as e:
        raise utils.MergifyError(
            f"Failed to fetch validation schema: {e}",
            exit_code=ExitCode.MERGIFY_API_ERROR,
        ) from e
    except (ValueError, TypeError) as e:
        raise utils.MergifyError(
            f"Failed to parse validation schema: {e}",
            exit_code=ExitCode.GENERIC_ERROR,
        ) from e

    escaped_path = escape(config_path)

    if result.is_valid:
        console.print(f"[green]Configuration file '{escaped_path}' is valid.[/]")
        return

    console_error(
        f"configuration file '{config_path}' has {len(result.errors)} error(s):",
    )
    for error in result.errors:
        console.print(
            f"  - {error.path}: {error.message}",
            style="red",
            markup=False,
        )

    raise utils.MergifyError(
        "configuration validation failed",
        exit_code=ExitCode.CONFIGURATION_ERROR,
    )


_PR_URL_RE = re.compile(
    r"https?://[^/]+/(?P<owner>[^/]+)/(?P<repo>[^/]+)/pull/(?P<number>\d+)$",
)


def _parse_pr_url(url: str) -> tuple[str, int]:
    m = _PR_URL_RE.match(url)
    if not m:
        msg = f"Invalid pull request URL: {url}"
        raise click.BadParameter(msg)
    return f"{m.group('owner')}/{m.group('repo')}", int(m.group("number"))


@config.command(
    help="Simulate Mergify actions on a pull request using the local configuration",
)
@click.argument("pull_request_url")
@click.option(
    "--token",
    "-t",
    help="Mergify or GitHub token",
    envvar=["MERGIFY_TOKEN", "GITHUB_TOKEN"],
    required=True,
    default=lambda: asyncio.run(utils.get_default_token()),
)
@click.option(
    "--api-url",
    "-u",
    help="URL of the Mergify API",
    envvar="MERGIFY_API_URL",
    default=utils.MERGIFY_API_DEFAULT_URL,
    show_default=True,
)
@click.pass_context
@utils.run_with_asyncio
async def simulate(
    ctx: click.Context,
    *,
    pull_request_url: str,
    token: str,
    api_url: str,
) -> None:
    repository, pull_number = _parse_pr_url(pull_request_url)
    config_path = _resolve_config_path(ctx.obj["config_file"])
    mergify_yml = config_validate.read_raw(config_path)

    async with utils.get_mergify_http_client(api_url, token) as client:
        result = await config_validate.simulate_pr(
            client,
            repository,
            pull_number,
            mergify_yml,
        )

    console.print(f"[bold]{escape(result.title)}[/]")
    console.print(Markdown(result.summary))
