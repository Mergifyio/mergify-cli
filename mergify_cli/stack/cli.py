import asyncio
import os
from urllib import parse

import click
import click_default_group

from mergify_cli import console
from mergify_cli import utils
from mergify_cli.stack import checkout as stack_checkout_mod
from mergify_cli.stack import edit as stack_edit_mod
from mergify_cli.stack import (
    github_action_auto_rebase as stack_github_action_auto_rebase_mod,
)
from mergify_cli.stack import push as stack_push_mod
from mergify_cli.stack import setup as stack_setup_mod


def trunk_type(
    _ctx: click.Context,
    _param: click.Parameter,
    value: str,
) -> tuple[str, str]:
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
            console.print(
                "error: please make sure that gh client is installed and you are authenticated, or set the "
                "'GITHUB_TOKEN' environment variable",
            )
    if utils.is_debug():
        console.print(f"[purple]DEBUG: token: {token}[/]")
    return token


def token_to_context(ctx: click.Context, _param: click.Parameter, value: str) -> None:
    ctx.obj["token"] = value


def github_server_to_context(
    ctx: click.Context,
    _param: click.Parameter,
    value: str,
) -> None:
    ctx.obj["github_server"] = value


stack = click_default_group.DefaultGroup(
    "stack",
    default="push",
    default_if_no_args=True,
    help="Manage pull requests stack",
    params=[
        click.Option(
            param_decls=["--token"],
            default=lambda: asyncio.run(get_default_token()),
            help="GitHub personal access token",
            callback=token_to_context,
        ),
        click.Option(
            param_decls=["--github-server"],
            default=lambda: asyncio.run(get_default_github_server()),
            help="GitHub API server",
            callback=github_server_to_context,
        ),
    ],
)


@stack.command(help="Configure the required git commit-msg hooks")
@utils.run_with_asyncio
async def setup() -> None:
    await stack_setup_mod.stack_setup()


@stack.command(help="Edit the stack history")
@utils.run_with_asyncio
async def edit() -> None:
    await stack_edit_mod.stack_edit()


@stack.command(help="Push/sync the pull requests stack")
@click.pass_context
@click.option(
    "--setup",
    is_flag=True,
    hidden=True,
)
@click.option("--dry-run", "-n", is_flag=True, default=False, help="dry run")
@click.option(
    "--next-only",
    "-x",
    is_flag=True,
    help="Only rebase and update the next pull request of the stack",
)
@click.option(
    "--skip-rebase",
    "-R",
    is_flag=True,
    help="Skip stack rebase",
)
@click.option(
    "--draft",
    "-d",
    is_flag=True,
    help="Create stacked pull request as draft",
)
@click.option(
    "--keep-pull-request-title-and-body",
    "-k",
    is_flag=True,
    # NOTE: `flag_value` here is used to allow the default's lazy loading with `is_flag`
    flag_value=True,
    default=lambda: asyncio.run(utils.get_default_keep_pr_title_body()),
    help="Don't update the title and body of already opened pull requests. "
    "Default fetched from git config if added with `git config --add mergify-cli.stack-keep-pr-title-body true`",
)
@click.option(
    "--author",
    help="Set the author of the stack (default: the author of the token)",
)
@click.option(
    "--trunk",
    "-t",
    type=click.UNPROCESSED,
    default=lambda: asyncio.run(utils.get_trunk()),
    callback=trunk_type,
    help="Change the target branch of the stack.",
)
@click.option(
    "--branch-prefix",
    default=None,
    help="Branch prefix used to create stacked PR. "
    "Default fetched from git config if added with `git config --add mergify-cli.stack-branch-prefix some-prefix`",
)
@click.option(
    "--only-update-existing-pulls",
    "-u",
    is_flag=True,
    help="Only update existing pull requests, do not create new ones",
)
@utils.run_with_asyncio
async def push(  # noqa: PLR0913, PLR0917
    ctx: click.Context,
    setup: bool,
    dry_run: bool,
    next_only: bool,
    skip_rebase: bool,
    draft: bool,
    keep_pull_request_title_and_body: bool,
    author: str,
    trunk: tuple[str, str],
    branch_prefix: str | None,
    only_update_existing_pulls: bool,
) -> None:
    if setup:
        # backward compat
        await stack_setup_mod.stack_setup()
        return

    await stack_push_mod.stack_push(
        github_server=ctx.obj["github_server"],
        token=ctx.obj["token"],
        skip_rebase=skip_rebase,
        next_only=next_only,
        branch_prefix=branch_prefix,
        dry_run=dry_run,
        trunk=trunk,
        create_as_draft=draft,
        keep_pull_request_title_and_body=keep_pull_request_title_and_body,
        only_update_existing_pulls=only_update_existing_pulls,
        author=author,
    )


@stack.command(help="Checkout the pull requests stack")
@click.pass_context
@click.option(
    "--author",
    help="Set the author of the stack (default: the author of the token)",
)
@click.option(
    "--repository",
    "--repo",
    help="Set the repository where the stack is located (eg: owner/repo)",
)
@click.option(
    "--branch",
    help="Branch used to create stacked PR.",
)
@click.option(
    "--branch-prefix",
    default=None,
    help="Branch prefix used to create stacked PR. "
    "Default fetched from git config if added with `git config --add mergify-cli.stack-branch-prefix some-prefix`",
)
@click.option(
    "--dry-run",
    "-n",
    is_flag=True,
    help="Only show what is going to be done",
)
@click.option(
    "--trunk",
    "-t",
    type=click.UNPROCESSED,
    default=lambda: asyncio.run(utils.get_trunk()),
    callback=trunk_type,
    help="Change the target branch of the stack.",
)
@utils.run_with_asyncio
async def checkout(  # noqa: PLR0913, PLR0917
    ctx: click.Context,
    author: str | None,
    repository: str,
    branch: str,
    branch_prefix: str | None,
    dry_run: bool,
    trunk: tuple[str, str],
) -> None:
    user, repo = repository.split("/")
    await stack_checkout_mod.stack_checkout(
        ctx.obj["github_server"],
        ctx.obj["token"],
        user,
        repo,
        branch_prefix,
        branch,
        author,
        trunk,
        dry_run,
    )


@stack.command(help="Autorebase a pull requests stack")
@click.pass_context
@utils.run_with_asyncio
async def github_action_auto_rebase(ctx: click.Context) -> None:
    await stack_github_action_auto_rebase_mod.stack_github_action_auto_rebase(
        ctx.obj["github_server"],
        ctx.obj["token"],
    )
