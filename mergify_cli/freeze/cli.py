from __future__ import annotations

import asyncio
import datetime
import os
import typing

import click
from rich.table import Table

from mergify_cli import console
from mergify_cli import utils
from mergify_cli.freeze import api as freeze_api


if typing.TYPE_CHECKING:
    import uuid


async def _get_default_token() -> str | None:
    token = os.environ.get("MERGIFY_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if not token:
        try:
            token = await utils.run_command("gh", "auth", "token")
        except utils.CommandError:
            console.print(
                "error: please set the 'MERGIFY_TOKEN' or 'GITHUB_TOKEN' environment variable, "
                "or make sure that gh client is installed and you are authenticated",
                style="red",
            )
            return None
    return token


async def _get_default_repository() -> str | None:
    repo = os.environ.get("GITHUB_REPOSITORY")
    if repo:
        return repo

    try:
        remote_url = await utils.git(
            "config",
            "--get",
            "remote.origin.url",
        )
    except utils.CommandError:
        return None

    try:
        user, repo_name = utils.get_slug(remote_url)
    except (ValueError, IndexError):
        return None

    return f"{user}/{repo_name}"


def _parse_naive_datetime(value: str) -> datetime.datetime:
    try:
        return datetime.datetime.fromisoformat(value)
    except ValueError:
        msg = f"Invalid datetime format: {value!r}. Use ISO 8601 format (e.g. 2024-06-19T08:00:00)"
        raise click.BadParameter(msg) from None


class NaiveDateTimeType(click.ParamType):
    name = "DATETIME"

    def convert(
        self,
        value: str | datetime.datetime,
        param: click.Parameter | None,  # noqa: ARG002
        ctx: click.Context | None,  # noqa: ARG002
    ) -> datetime.datetime:
        if isinstance(value, datetime.datetime):
            return value
        return _parse_naive_datetime(value)


NAIVE_DATETIME = NaiveDateTimeType()


def _format_datetime(
    value: str | None,
    timezone: str,
) -> str:
    if value is None:
        return "-"
    return f"{value} ({timezone})"


def _is_active(freeze: dict[str, typing.Any]) -> bool:
    start = freeze.get("start")
    if start is None:
        return False
    start_dt = datetime.datetime.fromisoformat(str(start))
    # NOTE: start is naive in the freeze's timezone, but we don't know the
    # server's current time in that timezone. We display the status based on
    # UTC as a best-effort approximation.
    return start_dt <= datetime.datetime.now(tz=datetime.UTC).replace(tzinfo=None)


def _print_freeze_table(freezes: list[dict[str, typing.Any]]) -> None:
    if not freezes:
        console.print("No scheduled freezes found.")
        return

    table = Table(title="Scheduled Freezes")
    table.add_column("ID", style="dim")
    table.add_column("Reason")
    table.add_column("Start")
    table.add_column("End")
    table.add_column("Conditions")
    table.add_column("Status")

    for freeze in freezes:
        timezone = str(freeze.get("timezone", ""))
        active = _is_active(freeze)
        conditions = ", ".join(
            str(c) for c in (freeze.get("matching_conditions") or [])
        )
        exclude = freeze.get("exclude_conditions") or []
        if exclude:
            conditions += f" (exclude: {', '.join(str(c) for c in exclude)})"

        table.add_row(
            str(freeze.get("id", "")),
            str(freeze.get("reason", "")),
            _format_datetime(
                str(freeze["start"]) if freeze.get("start") else None,
                timezone,
            ),
            _format_datetime(
                str(freeze["end"]) if freeze.get("end") else None,
                timezone,
            ),
            conditions,
            "[green]active[/]" if active else "[yellow]scheduled[/]",
        )

    console.print(table)


def _print_freeze(freeze: dict[str, typing.Any]) -> None:
    timezone = str(freeze.get("timezone", ""))
    console.print(f"  ID:         {freeze.get('id')}")
    console.print(f"  Reason:     {freeze.get('reason')}")
    console.print(
        f"  Start:      {_format_datetime(str(freeze['start']) if freeze.get('start') else None, timezone)}",
    )
    console.print(
        f"  End:        {_format_datetime(str(freeze['end']) if freeze.get('end') else None, timezone)}",
    )
    conditions = ", ".join(str(c) for c in (freeze.get("matching_conditions") or []))
    console.print(f"  Conditions: {conditions}")
    exclude = freeze.get("exclude_conditions") or []
    if exclude:
        console.print(
            f"  Exclude:    {', '.join(str(c) for c in exclude)}",
        )


@click.group(
    invoke_without_command=True,
    help="Manage scheduled freezes",
)
@click.option(
    "--token",
    "-t",
    help="Mergify or GitHub token",
    envvar=["MERGIFY_TOKEN", "GITHUB_TOKEN"],
    required=True,
    default=lambda: asyncio.run(_get_default_token()),
)
@click.option(
    "--api-url",
    "-u",
    help="URL of the Mergify API",
    envvar="MERGIFY_API_URL",
    default="https://api.mergify.com",
    show_default=True,
)
@click.option(
    "--repository",
    "-r",
    help="Repository full name (owner/repo)",
    required=True,
    default=lambda: asyncio.run(_get_default_repository()),
)
@click.pass_context
def freeze(
    ctx: click.Context,
    *,
    token: str,
    api_url: str,
    repository: str,
) -> None:
    ctx.ensure_object(dict)
    ctx.obj["token"] = token
    ctx.obj["api_url"] = api_url
    ctx.obj["repository"] = repository
    if ctx.invoked_subcommand is None:
        click.echo(ctx.get_help())


@freeze.command(name="list", help="List scheduled freezes for a repository")
@click.option(
    "--json",
    "output_json",
    is_flag=True,
    help="Output in JSON format",
)
@click.pass_context
@utils.run_with_asyncio
async def list_cmd(ctx: click.Context, *, output_json: bool) -> None:
    import json

    async with utils.get_mergify_http_client(
        ctx.obj["api_url"],
        ctx.obj["token"],
    ) as client:
        freezes = await freeze_api.list_freezes(client, ctx.obj["repository"])

    if output_json:
        click.echo(json.dumps(freezes, indent=2))
    else:
        _print_freeze_table(freezes)


@freeze.command(help="Create a new scheduled freeze")
@click.option("--reason", required=True, help="Reason for the freeze")
@click.option(
    "--timezone",
    required=True,
    help="IANA timezone name (e.g. Europe/Paris, US/Eastern)",
)
@click.option(
    "--condition",
    "-c",
    "conditions",
    multiple=True,
    required=True,
    help="Matching condition (repeatable, e.g. -c 'base=main')",
)
@click.option(
    "--start",
    type=NAIVE_DATETIME,
    default=None,
    help="Start time in ISO 8601 format (default: now)",
)
@click.option(
    "--end",
    type=NAIVE_DATETIME,
    default=None,
    help="End time in ISO 8601 format (default: no end / emergency freeze)",
)
@click.option(
    "--exclude",
    "-e",
    "excludes",
    multiple=True,
    help="Exclude condition (repeatable, e.g. -e 'label=hotfix')",
)
@click.pass_context
@utils.run_with_asyncio
async def create(
    ctx: click.Context,
    *,
    reason: str,
    timezone: str,
    conditions: tuple[str, ...],
    start: datetime.datetime | None,
    end: datetime.datetime | None,
    excludes: tuple[str, ...],
) -> None:
    async with utils.get_mergify_http_client(
        ctx.obj["api_url"],
        ctx.obj["token"],
    ) as client:
        result = await freeze_api.create_freeze(
            client,
            ctx.obj["repository"],
            reason=reason,
            timezone=timezone,
            matching_conditions=list(conditions),
            start=start,
            end=end,
            exclude_conditions=list(excludes) if excludes else None,
        )

    console.print("[green]Freeze created successfully:[/]")
    _print_freeze(result)


@freeze.command(help="Update an existing scheduled freeze")
@click.argument("freeze_id", type=click.UUID)
@click.option("--reason", required=True, help="Reason for the freeze")
@click.option(
    "--timezone",
    required=True,
    help="IANA timezone name (e.g. Europe/Paris, US/Eastern)",
)
@click.option(
    "--condition",
    "-c",
    "conditions",
    multiple=True,
    required=True,
    help="Matching condition (repeatable, e.g. -c 'base=main')",
)
@click.option(
    "--start",
    type=NAIVE_DATETIME,
    default=None,
    help="Start time in ISO 8601 format",
)
@click.option(
    "--end",
    type=NAIVE_DATETIME,
    default=None,
    help="End time in ISO 8601 format",
)
@click.option(
    "--exclude",
    "-e",
    "excludes",
    multiple=True,
    help="Exclude condition (repeatable, e.g. -e 'label=hotfix')",
)
@click.pass_context
@utils.run_with_asyncio
async def update(
    ctx: click.Context,
    *,
    freeze_id: uuid.UUID,
    reason: str,
    timezone: str,
    conditions: tuple[str, ...],
    start: datetime.datetime | None,
    end: datetime.datetime | None,
    excludes: tuple[str, ...],
) -> None:
    async with utils.get_mergify_http_client(
        ctx.obj["api_url"],
        ctx.obj["token"],
    ) as client:
        result = await freeze_api.update_freeze(
            client,
            ctx.obj["repository"],
            freeze_id,
            reason=reason,
            timezone=timezone,
            matching_conditions=list(conditions),
            start=start,
            end=end,
            exclude_conditions=list(excludes) if excludes else None,
        )

    console.print("[green]Freeze updated successfully:[/]")
    _print_freeze(result)


@freeze.command(help="Delete a scheduled freeze")
@click.argument("freeze_id", type=click.UUID)
@click.option(
    "--reason",
    "delete_reason",
    default=None,
    help="Reason for deleting the freeze (required if freeze is active)",
)
@click.pass_context
@utils.run_with_asyncio
async def delete(
    ctx: click.Context,
    *,
    freeze_id: uuid.UUID,
    delete_reason: str | None,
) -> None:
    async with utils.get_mergify_http_client(
        ctx.obj["api_url"],
        ctx.obj["token"],
    ) as client:
        await freeze_api.delete_freeze(
            client,
            ctx.obj["repository"],
            freeze_id,
            delete_reason=delete_reason,
        )

    console.print("[green]Freeze deleted successfully.[/]")
