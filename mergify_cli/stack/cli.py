from __future__ import annotations

import asyncio
import os
from urllib import parse

import click

from mergify_cli import console
from mergify_cli import console_error
from mergify_cli import utils
from mergify_cli.dym import DYMGroup


def trunk_type(
    _ctx: click.Context,
    _param: click.Parameter,
    value: str | None,
) -> tuple[str, str] | None:
    if value is None:
        return None
    result = value.split("/", maxsplit=1)
    if len(result) != 2:
        msg = "Trunk is invalid. It must be origin/branch-name [/]"
        raise click.BadParameter(msg)
    return result[0], result[1]


async def get_default_github_server() -> str:
    try:
        result = await utils.git("config", "--get", "mergify-cli.github-server")
    except utils.CommandError:
        result = ""

    url = parse.urlparse(result or "https://api.github.com/")
    url = url._replace(scheme="https")

    if url.hostname == "api.github.com":
        url = url._replace(path="")
    else:
        url = url._replace(path="/api/v3")
    return url.geturl()


async def get_default_token() -> str:
    token = os.environ.get("GITHUB_TOKEN", "")
    if not token:
        try:
            token = await utils.run_command("gh", "auth", "token")
        except utils.CommandError:
            console_error(
                "please make sure that gh client is installed and you are authenticated, or set the "
                "'GITHUB_TOKEN' environment variable",
            )
    if utils.is_debug():
        console.print(f"[purple]DEBUG: token: {token}[/]")
    return token


def token_to_context(ctx: click.Context, _param: click.Parameter, value: str) -> None:
    if ctx.obj is None:
        ctx.obj = {}
    ctx.obj["token"] = value


def github_server_to_context(
    ctx: click.Context,
    _param: click.Parameter,
    value: str,
) -> None:
    if ctx.obj is None:
        ctx.obj = {}
    ctx.obj["github_server"] = value


@click.group(
    cls=DYMGroup,
    invoke_without_command=True,
    help="Manage pull requests stack",
)
@click.option(
    "--token",
    default=lambda: asyncio.run(get_default_token()),
    help="GitHub personal access token",
    callback=token_to_context,
)
@click.option(
    "--github-server",
    default=lambda: asyncio.run(get_default_github_server()),
    help="GitHub API server",
    callback=github_server_to_context,
)
@click.pass_context
def stack(ctx: click.Context, **_kwargs: object) -> None:
    # Every `stack <subcommand>` is now served natively by the
    # `mergify` Rust binary; this click group exists only so
    # `python -m mergify_cli stack` still shows a sensible help
    # message rather than crashing on an unresolved import.
    if ctx.invoked_subcommand is None:
        click.echo(ctx.get_help())
