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

import argparse
import asyncio
import os
import sys
import typing
from urllib import parse

from mergify_cli import VERSION
from mergify_cli import console
from mergify_cli import utils
from mergify_cli.stack import checkout
from mergify_cli.stack import edit
from mergify_cli.stack import github_action_auto_rebase
from mergify_cli.stack import push
from mergify_cli.stack import setup


def trunk_type(trunk: str) -> tuple[str, str]:
    result = trunk.split("/", maxsplit=1)
    if len(result) != 2:
        msg = "Trunk is invalid. It must be origin/branch-name [/]"
        raise argparse.ArgumentTypeError(msg)
    return result[0], result[1]


def GitHubToken(v: str) -> str:  # noqa: N802
    if not v:
        raise ValueError
    return v


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


async def get_default_keep_pr_title_body() -> bool:
    try:
        result = await utils.git(
            "config",
            "--get",
            "mergify-cli.stack-keep-pr-title-body",
        )
    except utils.CommandError:
        return False

    return result == "true"


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


async def _stack_push(args: argparse.Namespace) -> None:
    if args.setup:
        # backward compat
        await setup.stack_setup()
        return

    await push.stack_push(
        args.github_server,
        args.token,
        args.skip_rebase,
        args.next_only,
        args.branch_prefix,
        args.dry_run,
        args.trunk,
        args.draft,
        args.keep_pull_request_title_and_body,
        args.only_update_existing_pulls,
        args.author,
    )


async def _stack_checkout(args: argparse.Namespace) -> None:
    user, repo = args.repository.split("/")

    await checkout.stack_checkout(
        args.github_server,
        args.token,
        user,
        repo,
        args.branch_prefix,
        args.branch,
        args.author,
        args.trunk,
        args.dry_run,
    )


def register_stack_setup_parser(
    sub_parsers: argparse._SubParsersAction[typing.Any],
) -> None:
    parser = sub_parsers.add_parser(
        "setup",
        description="Configure the git hooks",
        help="Initial installation of the required git commit-msg hook",
    )
    parser.set_defaults(func=lambda _: setup.stack_setup)


def register_stack_edit_parser(
    sub_parsers: argparse._SubParsersAction[typing.Any],
) -> None:
    parser = sub_parsers.add_parser(
        "edit",
        description="Edit the stack history",
        help="Edit the stack history",
    )
    parser.set_defaults(func=lambda _: edit.stack_edit)


async def _stack_github_action_auto_rebase(args: argparse.Namespace) -> None:
    await github_action_auto_rebase.stack_github_action_auto_rebase(
        args.github_server,
        args.token,
    )


def register_stack_github_action_autorebase(
    sub_parsers: argparse._SubParsersAction[typing.Any],
) -> None:
    parser = sub_parsers.add_parser(
        "github-action-auto-rebase",
        description="Autorebase a pull requests stack",
        help="Checkout a pull requests stack",
    )
    parser.set_defaults(func=_stack_github_action_auto_rebase)


async def register_stack_checkout_parser(
    sub_parsers: argparse._SubParsersAction[typing.Any],
) -> None:
    parser = sub_parsers.add_parser(
        "checkout",
        description="Checkout a pull requests stack",
        help="Checkout a pull requests stack",
    )
    parser.set_defaults(func=_stack_checkout)
    parser.add_argument(
        "--author",
        help="Set the author of the stack (default: the author of the token)",
    )
    parser.add_argument(
        "--repository",
        "--repo",
        help="Set the repository where the stack is located (eg: owner/repo)",
    )
    parser.add_argument(
        "--branch",
        help="Branch used to create stacked PR.",
    )
    parser.add_argument(
        "--branch-prefix",
        default=None,
        help="Branch prefix used to create stacked PR. "
        "Default fetched from git config if added with `git config --add mergify-cli.stack-branch-prefix some-prefix`",
    )
    parser.add_argument(
        "--dry-run",
        "-n",
        action="store_true",
        help="Only show what is going to be done",
    )
    parser.add_argument(
        "--trunk",
        "-t",
        type=trunk_type,
        default=await utils.get_trunk(),
        help="Change the target branch of the stack.",
    )


async def register_stack_push_parser(
    sub_parsers: argparse._SubParsersAction[typing.Any],
) -> None:
    parser = sub_parsers.add_parser(
        "push",
        description="Push/sync the pull requests stack",
        help="Push/sync the pull requests stack",
    )
    parser.set_defaults(func=_stack_push)

    # Backward compat
    parser.add_argument(
        "--setup",
        action="store_true",
        help="Initial installation of the required git commit-msg hook",
    )

    parser.add_argument(
        "--dry-run",
        "-n",
        action="store_true",
        help="Only show what is going to be done",
    )
    parser.add_argument(
        "--next-only",
        "-x",
        action="store_true",
        help="Only rebase and update the next pull request of the stack",
    )
    parser.add_argument(
        "--skip-rebase",
        "-R",
        action="store_true",
        help="Skip stack rebase",
    )
    parser.add_argument(
        "--draft",
        "-d",
        action="store_true",
        help="Create stacked pull request as draft",
    )
    parser.add_argument(
        "--keep-pull-request-title-and-body",
        "-k",
        action="store_true",
        default=await get_default_keep_pr_title_body(),
        help="Don't update the title and body of already opened pull requests. "
        "Default fetched from git config if added with `git config --add mergify-cli.stack-keep-pr-title-body true`",
    )
    parser.add_argument(
        "--author",
        help="Set the author of the stack (default: the author of the token)",
    )

    parser.add_argument(
        "--trunk",
        "-t",
        type=trunk_type,
        default=await utils.get_trunk(),
        help="Change the target branch of the stack.",
    )
    parser.add_argument(
        "--branch-prefix",
        default=None,
        help="Branch prefix used to create stacked PR. "
        "Default fetched from git config if added with `git config --add mergify-cli.stack-branch-prefix some-prefix`",
    )
    parser.add_argument(
        "--only-update-existing-pulls",
        "-u",
        action="store_true",
        help="Only update existing pull requests, do not create new ones",
    )


async def parse_args(args: typing.MutableSequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--version",
        "-V",
        action="version",
        version=f"%(prog)s {VERSION}",
        help="display version",
    )
    parser.add_argument("--debug", action="store_true", help="debug mode")
    parser.add_argument(
        "--token",
        default=await get_default_token(),
        type=GitHubToken,
        help="GitHub personal access token",
    )
    parser.add_argument("--dry-run", "-n", action="store_true")
    parser.add_argument(
        "--github-server",
        action="store_true",
        default=await get_default_github_server(),
    )

    sub_parsers = parser.add_subparsers(dest="action")

    stack_parser = sub_parsers.add_parser(
        "stack",
        description="Stacked Pull Requests CLI",
        help="Create a pull requests stack",
    )
    stack_sub_parsers = stack_parser.add_subparsers(dest="stack_action")
    await register_stack_push_parser(stack_sub_parsers)
    await register_stack_checkout_parser(stack_sub_parsers)
    register_stack_edit_parser(stack_sub_parsers)
    register_stack_setup_parser(stack_sub_parsers)
    register_stack_github_action_autorebase(stack_sub_parsers)

    known_args, _ = parser.parse_known_args(args)

    # Default
    if known_args.action is None:
        args.insert(0, "stack")

    known_args, _ = parser.parse_known_args(args)

    if known_args.action == "stack" and known_args.stack_action is None:
        args.insert(1, "push")

    return parser.parse_args(args)


async def async_main() -> None:
    args = await parse_args(sys.argv[1:])
    utils.set_debug(args.debug)
    await args.func(args)


def main() -> None:
    asyncio.run(async_main())
