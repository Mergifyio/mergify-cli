from __future__ import annotations

import pathlib

import click
import httpx
from rich.markup import escape
import yaml

from mergify_cli import console
from mergify_cli.ci.detector import MERGIFY_CONFIG_PATHS
from mergify_cli.ci.detector import get_mergify_config_path
from mergify_cli.config import validate as config_validate


@click.group(
    invoke_without_command=True,
    help="Manage Mergify configuration",
)
@click.pass_context
def config(ctx: click.Context) -> None:
    ctx.ensure_object(dict)
    if ctx.invoked_subcommand is None:
        click.echo(ctx.get_help())


@config.command(help="Validate the Mergify configuration file against the schema")
@click.option(
    "--config",
    "config_path",
    type=click.Path(dir_okay=False),
    default=None,
    help="Path to the Mergify configuration file (auto-detected if not provided)",
)
def validate(*, config_path: str | None) -> None:
    if config_path is None:
        config_path = get_mergify_config_path()

    if config_path is None:
        locations = ", ".join(MERGIFY_CONFIG_PATHS)
        msg = f"Mergify configuration file not found. Looked in: {locations}"
        raise click.ClickException(msg)

    if not pathlib.Path(config_path).is_file():
        msg = f"Configuration file not found: {config_path}"
        raise click.ClickException(msg)

    try:
        config_data = config_validate.load_yaml(config_path)
    except yaml.YAMLError as e:
        raise click.ClickException(f"Invalid YAML in {config_path}: {e}") from e
    except TypeError as e:
        raise click.ClickException(str(e)) from e

    try:
        with httpx.Client(timeout=30) as client:
            schema = config_validate.fetch_schema(client)
    except httpx.HTTPError as e:
        raise click.ClickException(f"Failed to fetch validation schema: {e}") from e

    result = config_validate.validate_config(config_data, schema)

    escaped_path = escape(config_path)

    if result.is_valid:
        console.print(f"[green]Configuration file '{escaped_path}' is valid.[/]")
        return

    console.print(
        f"[red]Configuration file '{escaped_path}' has {len(result.errors)} error(s):[/]",
    )
    for error in result.errors:
        console.print(
            f"  [red]- {escape(error.path)}: {escape(error.message)}[/]",
        )

    raise SystemExit(1)
