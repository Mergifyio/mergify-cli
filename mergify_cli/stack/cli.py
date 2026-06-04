from __future__ import annotations

import asyncio
import os
from urllib import parse

import click

from mergify_cli import console
from mergify_cli import console_error
from mergify_cli import utils
from mergify_cli.dym import DYMGroup
from mergify_cli.stack import push as stack_push_mod


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
    if ctx.invoked_subcommand is None:
        click.echo(ctx.get_help())


@stack.command(
    help=(
        "Push/sync the pull requests stack. "
        "By default, `stack push` skips the rebase when any PR in the stack "
        "has approvals; use --force-rebase to rebase anyway."
    ),
)
@click.pass_context
@click.option(
    "--setup",
    is_flag=True,
    hidden=True,
)
@click.option(
    "--no-upgrade-hooks",
    is_flag=True,
    help="Skip automatic hook script upgrades",
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
    "--force-rebase",
    is_flag=True,
    help="Rebase the stack even if PRs have approvals "
    "(mutually exclusive with --skip-rebase)",
)
@click.option(
    "--draft",
    "-d",
    is_flag=True,
    # NOTE: `flag_value` here is used to allow the default's lazy loading with `is_flag`
    flag_value=True,
    default=lambda: asyncio.run(utils.get_default_create_as_draft()),
    help="Create stacked pull request as draft. "
    "Default fetched from git config if added with `git config --add mergify-cli.stack-create-as-draft true`",
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
@click.option(
    "--no-revision-history",
    is_flag=True,
    flag_value=True,
    default=lambda: not asyncio.run(utils.get_default_revision_history()),
    help="Don't post revision history comments on pull requests. "
    "Default fetched from git config if added with "
    "`git config --add mergify-cli.stack-revision-history false`",
)
@click.option(
    "--no-verify",
    is_flag=True,
    default=False,
    help="Skip pre-push git hooks",
)
@utils.run_with_asyncio
async def push(
    ctx: click.Context,
    *,
    setup: bool,
    no_upgrade_hooks: bool,
    dry_run: bool,
    next_only: bool,
    skip_rebase: bool,
    force_rebase: bool,
    draft: bool,
    keep_pull_request_title_and_body: bool,
    author: str,
    trunk: tuple[str, str],
    branch_prefix: str | None,
    only_update_existing_pulls: bool,
    no_revision_history: bool,
    no_verify: bool,
) -> None:
    if skip_rebase and force_rebase:
        msg = "--skip-rebase and --force-rebase are mutually exclusive"
        raise click.UsageError(msg)

    if setup:
        # Backward-compat: ``--setup`` runs the hook installer and
        # exits. The Rust binary owns the installer now, so we
        # subprocess into it rather than re-importing the deleted
        # ``stack/setup.py``.
        await utils.run_command("mergify", "stack", "setup")
        return

    # Auto-upgrade managed hook scripts (not the user-modifiable
    # wrappers) unless ``--no-upgrade-hooks`` is set. Same subprocess
    # path as above — see the deleted ``ensure_hooks_updated`` for
    # the pre-port shape.
    if not no_upgrade_hooks:
        await utils.run_command("mergify", "stack", "setup")

    await stack_push_mod.stack_push(
        github_server=ctx.obj["github_server"],
        token=ctx.obj["token"],
        skip_rebase=skip_rebase,
        force_rebase=force_rebase,
        next_only=next_only,
        branch_prefix=branch_prefix,
        dry_run=dry_run,
        trunk=trunk,
        create_as_draft=draft,
        keep_pull_request_title_and_body=keep_pull_request_title_and_body,
        only_update_existing_pulls=only_update_existing_pulls,
        author=author,
        revision_history=not no_revision_history,
        no_verify=no_verify,
    )
