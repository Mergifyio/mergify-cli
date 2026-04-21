from __future__ import annotations

import asyncio
import os
from typing import Any
from urllib import parse

import click

from mergify_cli import console
from mergify_cli import console_error
from mergify_cli import utils
from mergify_cli.dym import DYMGroup
from mergify_cli.stack import checkout as stack_checkout_mod
from mergify_cli.stack import edit as stack_edit_mod
from mergify_cli.stack import list as stack_list_mod
from mergify_cli.stack import move as stack_move_mod
from mergify_cli.stack import new as stack_new_mod
from mergify_cli.stack import note as stack_note_mod
from mergify_cli.stack import open as stack_open_mod
from mergify_cli.stack import push as stack_push_mod
from mergify_cli.stack import reorder as stack_reorder_mod
from mergify_cli.stack import setup as stack_setup_mod
from mergify_cli.stack import squash as stack_squash_mod
from mergify_cli.stack import sync as stack_sync_mod


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
    ctx.obj["token"] = value


def github_server_to_context(
    ctx: click.Context,
    _param: click.Parameter,
    value: str,
) -> None:
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


def _print_hooks_status(status: dict[str, Any]) -> None:
    """Print hooks status in a formatted table."""
    needs_setup = False
    needs_force = False

    # Git hooks section
    console.print("\nGit Hooks Status:\n")
    git_hooks = status["git_hooks"]

    for hook_name, info in git_hooks.items():
        console.print(f"  {hook_name}:")

        wrapper_status = info["wrapper_status"]
        wrapper_path = info["wrapper_path"]

        if wrapper_status == stack_setup_mod.WrapperStatus.INSTALLED:
            console.print(f"    Wrapper: [green]installed[/] ({wrapper_path})")
        elif wrapper_status == stack_setup_mod.WrapperStatus.LEGACY:
            console.print(
                "    Wrapper: [yellow]legacy[/] (needs --force to migrate)",
            )
            needs_force = True
        else:  # MISSING
            console.print("    Wrapper: [red]not installed[/]")
            needs_setup = True

        script_path = info["script_path"]
        if info["script_installed"]:
            if info["script_needs_update"]:
                console.print(f"    Script:  [yellow]needs update[/] ({script_path})")
                needs_setup = True
            else:
                console.print(f"    Script:  [green]up to date[/] ({script_path})")
        else:
            console.print("    Script:  [red]not installed[/]")
            needs_setup = True

        console.print()

    if needs_setup or needs_force:
        console.print("Run 'mergify stack hooks --setup' to install/upgrade hooks.")
        if needs_force:
            console.print(
                "Run 'mergify stack hooks --setup --force' to force reinstall wrappers.",
            )
    else:
        console.print("[green]All hooks are up to date.[/]")


@stack.command(help="Show git hooks status and manage installation")
@click.option(
    "--setup",
    "do_setup",
    is_flag=True,
    help="Install or upgrade hooks",
)
@click.option(
    "--force",
    "-f",
    is_flag=True,
    help="Force reinstall wrappers (use with --setup)",
)
@utils.run_with_asyncio
async def hooks(*, do_setup: bool, force: bool) -> None:
    if do_setup:
        await stack_setup_mod.stack_setup(force=force)
    else:
        status = await stack_setup_mod.get_hooks_status()
        _print_hooks_status(status)


@stack.command(help="Configure git hooks (alias for 'stack hooks --setup')")
@click.option(
    "--force",
    "-f",
    is_flag=True,
    help="Force reinstall of hook wrappers, even if user modified them",
)
@click.option(
    "--check",
    is_flag=True,
    help="Check status only (use 'stack hooks' instead)",
)
@utils.run_with_asyncio
async def setup(*, force: bool, check: bool) -> None:
    if check:
        status = await stack_setup_mod.get_hooks_status()
        _print_hooks_status(status)
    else:
        await stack_setup_mod.stack_setup(force=force)


@stack.command(help="Edit the stack history")
@click.argument("commit", required=False, default=None)
@utils.run_with_asyncio
async def edit(*, commit: str | None) -> None:
    await stack_edit_mod.stack_edit(commit_prefix=commit)


@stack.command(help="Attach a 'why was this commit amended' note to a commit")
@click.argument("commit", required=False, default=None)
@click.option(
    "-m",
    "--message",
    "message",
    default=None,
    help="Note message. If omitted, opens $GIT_EDITOR.",
)
@click.option(
    "--append",
    "do_append",
    is_flag=True,
    help="Append to an existing note instead of replacing",
)
@click.option(
    "--remove",
    "do_remove",
    is_flag=True,
    help="Remove the note on the target commit",
)
@utils.run_with_asyncio
async def note(
    *,
    commit: str | None,
    message: str | None,
    do_append: bool,
    do_remove: bool,
) -> None:
    if do_remove and (message is not None or do_append):
        msg = "--remove cannot be combined with --message or --append"
        raise click.UsageError(msg)
    await stack_note_mod.stack_note(
        commit=commit,
        message=message,
        append=do_append,
        remove=do_remove,
    )


@stack.command(help="Reorder the stack's commits")
@click.argument("commits", nargs=-1, required=True)
@click.option(
    "--dry-run",
    "-n",
    is_flag=True,
    default=False,
    help="Show the plan without reordering",
)
@utils.run_with_asyncio
async def reorder(*, commits: tuple[str, ...], dry_run: bool) -> None:
    await stack_reorder_mod.stack_reorder(list(commits), dry_run=dry_run)


@stack.command(help="Move a commit within the stack")
@click.argument("commit")
@click.argument("position", type=click.Choice(["before", "after", "first", "last"]))
@click.argument("target", required=False, default=None)
@click.option(
    "--dry-run",
    "-n",
    is_flag=True,
    default=False,
    help="Show the plan without moving",
)
@utils.run_with_asyncio
async def move(
    *,
    commit: str,
    position: str,
    target: str | None,
    dry_run: bool,
) -> None:
    await stack_move_mod.stack_move(
        commit_prefix=commit,
        position=position,
        target_prefix=target,
        dry_run=dry_run,
    )


@stack.command(help="Create a new stack branch")
@click.argument("name")
@click.option(
    "--base",
    "-b",
    type=click.UNPROCESSED,
    metavar="REMOTE/BRANCH",
    default=None,
    callback=trunk_type,
    help="Base branch to create from (default: current trunk)",
)
@click.option(
    "--checkout/--no-checkout",
    default=True,
    help="Whether to checkout the new branch after creation (default: checkout)",
)
@utils.run_with_asyncio
async def new(
    *,
    name: str,
    base: tuple[str, str] | None,
    checkout: bool,
) -> None:
    await stack_new_mod.stack_new(
        name=name,
        base=base,
        checkout=checkout,
    )


@stack.command(help="Push/sync the pull requests stack")
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
    draft: bool,
    keep_pull_request_title_and_body: bool,
    author: str,
    trunk: tuple[str, str],
    branch_prefix: str | None,
    only_update_existing_pulls: bool,
    no_revision_history: bool,
    no_verify: bool,
) -> None:
    if setup:
        # backward compat
        await stack_setup_mod.stack_setup()
        return

    # Auto-upgrade hook scripts (not wrappers) unless disabled
    if not no_upgrade_hooks:
        await stack_setup_mod.ensure_hooks_updated()

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
        revision_history=not no_revision_history,
        no_verify=no_verify,
    )


@stack.command(help="Checkout the pull requests stack")
@click.pass_context
@click.argument("name")
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
    default=None,
    help="Local branch name to create. Default: same as NAME.",
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
async def checkout(
    ctx: click.Context,
    *,
    name: str,
    author: str | None,
    repository: str | None,
    branch: str | None,
    branch_prefix: str | None,
    dry_run: bool,
    trunk: tuple[str, str],
) -> None:
    remote, _base_branch = trunk
    if repository is not None:
        repository_parts = repository.split("/", maxsplit=1)
        if (
            len(repository_parts) != 2
            or not repository_parts[0]
            or not repository_parts[1]
        ):
            raise click.BadParameter(
                "Repository must be in the format 'owner/repo'",
                param_hint="--repository",
            )
        user, repo = repository_parts
    else:
        user, repo = utils.get_slug(
            await utils.git("config", "--get", f"remote.{remote}.url"),
        )
    await stack_checkout_mod.stack_checkout(
        ctx.obj["github_server"],
        ctx.obj["token"],
        user=user,
        repo=repo,
        branch_prefix=branch_prefix,
        name=name,
        branch=branch,
        author=author,
        trunk=trunk,
        dry_run=dry_run,
    )


@stack.command(help="Sync the stack: fetch trunk, remove merged commits, rebase")
@click.pass_context
@click.option(
    "--dry-run",
    "-n",
    is_flag=True,
    default=False,
    help="Show what would happen without making changes",
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
async def sync(
    ctx: click.Context,
    *,
    dry_run: bool,
    trunk: tuple[str, str],
) -> None:
    await stack_sync_mod.stack_sync(
        github_server=ctx.obj["github_server"],
        token=ctx.obj["token"],
        trunk=trunk,
        dry_run=dry_run,
    )


@stack.command(name="list", help="List the stack's commits and their associated PRs")
@click.pass_context
@click.option(
    "--trunk",
    "-t",
    type=click.UNPROCESSED,
    default=lambda: asyncio.run(utils.get_trunk()),
    callback=trunk_type,
    help="Change the target branch of the stack.",
)
@click.option(
    "--json",
    "output_json",
    is_flag=True,
    help="Output in JSON format for scripting",
)
@click.option(
    "--verbose",
    "-v",
    is_flag=True,
    help="Show detailed CI check names and reviewer names",
)
@utils.run_with_asyncio
async def list_cmd(
    ctx: click.Context,
    *,
    trunk: tuple[str, str],
    output_json: bool,
    verbose: bool,
) -> None:
    await stack_list_mod.stack_list(
        github_server=ctx.obj["github_server"],
        token=ctx.obj["token"],
        trunk=trunk,
        output_json=output_json,
        verbose=verbose,
    )


@stack.command(name="open", help="Open a PR from the stack in the browser")
@click.pass_context
@click.argument("commit", required=False, default=None)
@utils.run_with_asyncio
async def open_cmd(
    ctx: click.Context,
    *,
    commit: str | None,
) -> None:
    await stack_open_mod.stack_open(
        github_server=ctx.obj["github_server"],
        token=ctx.obj["token"],
        commit=commit,
    )


@stack.command(help="Fixup commits into their parent (drops their messages)")
@click.argument("commits", nargs=-1, required=True)
@click.option(
    "--dry-run",
    "-n",
    is_flag=True,
    default=False,
    help="Show the plan without rebasing",
)
@utils.run_with_asyncio
async def fixup(*, commits: tuple[str, ...], dry_run: bool) -> None:
    await stack_squash_mod.stack_fixup(list(commits), dry_run=dry_run)
