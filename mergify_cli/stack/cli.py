from __future__ import annotations

import asyncio
import os
from typing import Any
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
from mergify_cli.stack import list as stack_list_mod
from mergify_cli.stack import push as stack_push_mod
from mergify_cli.stack import session as stack_session_mod
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

    # Claude hooks section
    console.print("Claude Hooks Status:\n")
    claude_hooks = status["claude_hooks"]

    for script_name, script_info in claude_hooks["scripts"].items():
        console.print(f"  {script_name}:")
        if script_info["installed"]:
            if script_info["needs_update"]:
                console.print(
                    f"    Script: [yellow]needs update[/] ({script_info['path']})",
                )
                needs_setup = True
            else:
                console.print(
                    f"    Script: [green]up to date[/] ({script_info['path']})",
                )
        else:
            console.print("    Script: [red]not installed[/]")
            needs_setup = True
        console.print()

    console.print("  settings.json:")
    if claude_hooks["settings_installed"]:
        console.print(
            f"    Hook: [green]configured[/] ({claude_hooks['settings_path']})",
        )
    else:
        console.print("    Hook: [red]not configured[/]")
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


@stack.command(help="Show git hooks status and manage installation")  # type: ignore[untyped-decorator]
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


@stack.command(help="Configure git hooks (alias for 'stack hooks --setup')")  # type: ignore[untyped-decorator]
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


@stack.command(help="Edit the stack history")  # type: ignore[untyped-decorator]
@utils.run_with_asyncio
async def edit() -> None:
    await stack_edit_mod.stack_edit()


@stack.command(help="Push/sync the pull requests stack")  # type: ignore[untyped-decorator]
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
    )


@stack.command(help="Checkout the pull requests stack")  # type: ignore[untyped-decorator]
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
async def checkout(
    ctx: click.Context,
    *,
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
        user=user,
        repo=repo,
        branch_prefix=branch_prefix,
        branch=branch,
        author=author,
        trunk=trunk,
        dry_run=dry_run,
    )


@stack.command(help="Autorebase a pull requests stack")  # type: ignore[untyped-decorator]
@click.pass_context
@utils.run_with_asyncio
async def github_action_auto_rebase(ctx: click.Context) -> None:
    await stack_github_action_auto_rebase_mod.stack_github_action_auto_rebase(
        ctx.obj["github_server"],
        ctx.obj["token"],
    )


@stack.command(help="Get Claude session ID from a commit")  # type: ignore[untyped-decorator]
@click.option(
    "--commit",
    "-c",
    default="HEAD",
    help="Commit to extract session ID from (default: HEAD)",
)
@click.option(
    "--launch",
    "-l",
    is_flag=True,
    help="Launch Claude with the extracted session ID",
)
@utils.run_with_asyncio
async def session(*, commit: str, launch: bool) -> None:
    """Extract and optionally launch Claude session from commit."""
    session_id = await stack_session_mod.get_session_id_from_commit(commit)
    if session_id is None:
        console.print(f"No Claude-Session-Id found in commit {commit}", style="yellow")
        return

    console.print(f"Claude-Session-Id: {session_id}")

    if launch:
        stack_session_mod.launch_claude_session(session_id)


@stack.command(name="list", help="List the stack's commits and their associated PRs")  # type: ignore[untyped-decorator]
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
@utils.run_with_asyncio
async def list_cmd(
    ctx: click.Context,
    *,
    trunk: tuple[str, str],
    output_json: bool,
) -> None:
    await stack_list_mod.stack_list(
        github_server=ctx.obj["github_server"],
        token=ctx.obj["token"],
        trunk=trunk,
        output_json=output_json,
    )
