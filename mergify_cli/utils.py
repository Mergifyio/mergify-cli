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
import dataclasses
import functools
import sys
import typing
from urllib import parse

import httpx

from mergify_cli import VERSION
from mergify_cli import console


_DEBUG = False


def set_debug(debug: bool) -> None:
    global _DEBUG  # noqa: PLW0603
    _DEBUG = debug


def is_debug() -> bool:
    return _DEBUG


async def check_for_status(response: httpx.Response) -> None:
    if response.status_code < 400:
        return

    if response.status_code < 500:
        await response.aread()
        data = response.json()
        console.print(f"url: {response.request.url}", style="red")
        console.print(f"data: {response.request.content.decode()}", style="red")
        console.print(
            f"HTTPError {response.status_code}: {data['message']}",
            style="red",
        )
        if "errors" in data:
            console.print(
                "\n".join(f"* {e.get('message') or e}" for e in data["errors"]),
                style="red",
            )
        sys.exit(1)

    response.raise_for_status()


@dataclasses.dataclass
class CommandError(Exception):
    command_args: tuple[str, ...]
    returncode: int | None
    stdout: bytes

    def __str__(self) -> str:
        return f"failed to run `{' '.join(self.command_args)}`: {self.stdout.decode()}"


async def run_command(*args: str) -> str:
    if is_debug():
        console.print(f"[purple]DEBUG: running: git {' '.join(args)} [/]")
    proc = await asyncio.create_subprocess_exec(
        *args,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.STDOUT,
    )
    stdout, _ = await proc.communicate()
    if proc.returncode != 0:
        raise CommandError(args, proc.returncode, stdout)
    return stdout.decode().strip()


async def git(*args: str) -> str:
    return await run_command("git", *args)


async def git_get_branch_name() -> str:
    return await git("rev-parse", "--abbrev-ref", "HEAD")


async def git_get_target_branch(branch: str) -> str:
    return (await git("config", "--get", "branch." + branch + ".merge")).removeprefix(
        "refs/heads/",
    )


async def git_get_target_remote(branch: str) -> str:
    return await git("config", "--get", "branch." + branch + ".remote")


async def get_default_branch_prefix(author: str) -> str:
    try:
        result = await git("config", "--get", "mergify-cli.stack-branch-prefix")
    except CommandError:
        result = ""

    return result or f"stack/{author}"


async def get_default_keep_pr_title_body() -> bool:
    try:
        result = await git(
            "config",
            "--get",
            "mergify-cli.stack-keep-pr-title-body",
        )
    except CommandError:
        return False

    return result == "true"


async def get_trunk() -> str:
    try:
        branch_name = await git_get_branch_name()
    except CommandError:
        console.print("error: can't get the current branch", style="red")
        raise
    try:
        target_branch = await git_get_target_branch(branch_name)
    except CommandError:
        # It's possible this has not been set; ignore
        console.print("error: can't get the remote target branch", style="red")
        console.print(
            f"Please set the target branch with `git branch {branch_name} --set-upstream-to=<remote>/<branch>",
            style="red",
        )
        raise

    try:
        target_remote = await git_get_target_remote(branch_name)
    except CommandError:
        console.print(
            f"error: can't get the target remote for branch {branch_name}",
            style="red",
        )
        raise
    return f"{target_remote}/{target_branch}"


def get_slug(url: str) -> tuple[str, str]:
    parsed = parse.urlparse(url)
    if not parsed.netloc:
        # Probably ssh
        _, _, path = parsed.path.partition(":")
    else:
        path = parsed.path[1:].rstrip("/")

    user, repo = path.split("/", 1)
    repo = repo.removesuffix(".git")
    return user, repo


# NOTE: must be async for httpx
async def log_httpx_request(request: httpx.Request) -> None:  # noqa: RUF029
    console.print(
        f"[purple]DEBUG: request: {request.method} {request.url} - Waiting for response[/]",
    )


# NOTE: must be async for httpx
async def log_httpx_response(response: httpx.Response) -> None:
    request = response.request
    await response.aread()
    elapsed = response.elapsed.total_seconds()
    console.print(
        f"[purple]DEBUG: response: {request.method} {request.url} - Status {response.status_code} - Elasped {elapsed} s[/]",
    )


def get_github_http_client(github_server: str, token: str) -> httpx.AsyncClient:
    event_hooks: typing.Mapping[str, list[typing.Callable[..., typing.Any]]] = {
        "request": [],
        "response": [check_for_status],
    }
    if is_debug():
        event_hooks["request"].insert(0, log_httpx_request)
        event_hooks["response"].insert(0, log_httpx_response)

    return httpx.AsyncClient(
        base_url=github_server,
        headers={
            "Accept": "application/vnd.github.v3+json",
            "User-Agent": f"mergify_cli/{VERSION}",
            "Authorization": f"token {token}",
        },
        event_hooks=event_hooks,
        follow_redirects=True,
        timeout=5.0,
    )


P = typing.ParamSpec("P")
R = typing.TypeVar("R")


def run_with_asyncio(
    func: typing.Callable[
        P,
        typing.Coroutine[typing.Any, typing.Any, R],
    ],
) -> functools._Wrapped[
    P,
    typing.Coroutine[typing.Any, typing.Any, R],
    P,
    R,
]:
    @functools.wraps(func)
    def wrapper(*args: P.args, **kwargs: P.kwargs) -> R:
        result = func(*args, **kwargs)
        return asyncio.run(result)

    return wrapper
