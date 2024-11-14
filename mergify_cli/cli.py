#
#  Copyright © 2021-2024 Mergify SAS
#
# Licensed under the Apache License, Version 2.0 (the "License"); you may
# not use this file except in compliance with the License. You may obtain
# a copy of the License at
#
#      http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS, WITHOUT
# WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied. See the
# License for the specific language governing permissions and limitations
# under the License.

from __future__ import annotations

import asyncio
import os
from urllib import parse

import click
import click.decorators
import click_default_group

from mergify_cli import VERSION
from mergify_cli import console
from mergify_cli import utils
from mergify_cli.stack import cli as stack_cli_mod


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
            console.print(
                "error: please make sure that gh client is installed and you are authenticated, or set the "
                "'GITHUB_TOKEN' environment variable",
            )
    if utils.is_debug():
        console.print(f"[purple]DEBUG: token: {token}[/]")
    return token


@click.group(
    cls=click_default_group.DefaultGroup,
    default="stack",
    default_if_no_args=True,
)
@click.option("--debug", is_flag=True, default=False, help="debug mode")
@click.version_option(VERSION)
@click.option(
    "--github-server",
    default=asyncio.run(get_default_github_server()),
)
@click.option(
    "--token",
    default=asyncio.run(get_default_token()),
    help="GitHub personal access token",
)
@click.pass_context
def cli(
    ctx: click.Context,
    debug: bool,
    github_server: str,
    token: str,
) -> None:
    ctx.obj = {
        "debug": debug,
        "github_server": github_server,
        "token": token,
    }


cli.add_command(stack_cli_mod.stack)


def main() -> None:
    cli()
