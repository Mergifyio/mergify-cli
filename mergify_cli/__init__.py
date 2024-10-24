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
import contextlib
import dataclasses
import importlib.metadata
import os
import pathlib
import re
import shutil
import sys
import typing
from urllib import parse

import aiofiles
import httpx
import rich
import rich.console

from mergify_cli import github_types


VERSION = importlib.metadata.version("mergify-cli")

CHANGEID_RE = re.compile(r"Change-Id: (I[0-9a-z]{40})")
DEPENDS_ON_RE = re.compile(r"Depends-On: (#[0-9]*)")
console = rich.console.Console(log_path=False, log_time=False)

DEBUG = False
TMP_STACK_BRANCH = "mergify-cli-tmp"


def check_for_status(response: httpx.Response) -> None:
    if response.status_code < 400:
        return

    if response.status_code < 500:
        data = response.json()
        console.print(f"url: {response.request.url}", style="red")
        with contextlib.suppress(httpx.RequestNotRead):
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


async def _run_command(*args: str) -> str:
    if DEBUG:
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
    return await _run_command("git", *args)


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


async def stack_setup(_: argparse.Namespace) -> None:
    hooks_dir = pathlib.Path(await git("rev-parse", "--git-path", "hooks"))
    installed_hook_file = hooks_dir / "commit-msg"

    new_hook_file = str(
        importlib.resources.files(__package__).joinpath("hooks/commit-msg"),
    )

    if installed_hook_file.exists():
        async with aiofiles.open(installed_hook_file) as f:
            data_installed = await f.read()
        async with aiofiles.open(new_hook_file) as f:
            data_new = await f.read()
        if data_installed == data_new:
            console.log("Git commit-msg hook is up to date")
        else:
            console.print(
                f"error: {installed_hook_file} differ from mergify_cli hook",
                style="red",
            )
            sys.exit(1)

    else:
        console.log("Installation of git commit-msg hook")
        shutil.copy(new_hook_file, installed_hook_file)
        installed_hook_file.chmod(0o755)


ChangeId = typing.NewType("ChangeId", str)
RemoteChanges = typing.NewType(
    "RemoteChanges",
    dict[ChangeId, github_types.PullRequest | None],
)


async def get_change_id_and_pull_from_github(
    client: httpx.AsyncClient,
    user: str,
    stack_prefix: str,
    ref: github_types.GitRef,
) -> tuple[ChangeId, github_types.PullRequest | None]:
    branch = ref["ref"][len("refs/heads/") :]
    changeid = ChangeId(branch[len(stack_prefix) + 1 :])
    r = await client.get("pulls", params={"head": f"{user}:{branch}", "state": "all"})
    check_for_status(r)
    pulls = typing.cast(list[github_types.PullRequest], r.json())
    opened_pulls = [pull for pull in pulls if pull["state"] == "open"]
    merged_pulls = [pull for pull in pulls if pull["merged_at"] is not None]

    if opened_pulls:
        if len(opened_pulls) > 1:
            msg = f"More than 1 pull found with this head: {branch}"
            raise RuntimeError(msg)

        pull = opened_pulls[0]
    elif merged_pulls:
        pull = merged_pulls[0]
    else:
        return changeid, None

    if pull["body"] is None or "merged_at" not in pull:
        r = await client.get(f"pulls/{pull['number']}")
        check_for_status(r)
        pull = typing.cast(github_types.PullRequest, r.json())
    return changeid, pull


class PullRequestNotExistError(Exception):
    pass


@dataclasses.dataclass
class Change:
    id: ChangeId
    pull: github_types.PullRequest | None

    @property
    def pull_head_sha(self) -> str:
        if self.pull is None:
            raise PullRequestNotExistError
        return self.pull["head"]["sha"]

    @property
    def pull_short_head_sha(self) -> str:
        return self.pull_head_sha[:7]


ActionT = typing.Literal[
    "skip-merged",
    "skip-next-only",
    "skip-create",
    "skip-up-to-date",
    "create",
    "update",
]


@dataclasses.dataclass
class LocalChange(Change):
    commit_sha: str
    title: str
    message: str
    base_branch: str
    dest_branch: str
    action: ActionT

    @property
    def commit_short_sha(self) -> str:
        return self.commit_sha[:7]


@dataclasses.dataclass
class OrphanChange(Change):
    pass


@dataclasses.dataclass
class Changes:
    stack_prefix: str
    locals: list[LocalChange] = dataclasses.field(default_factory=list)
    orphans: list[OrphanChange] = dataclasses.field(default_factory=list)


async def get_changes(  # noqa: PLR0913,PLR0917
    base_commit_sha: str,
    stack_prefix: str,
    base_branch: str,
    dest_branch: str,
    remote_changes: RemoteChanges,
    only_update_existing_pulls: bool,
    next_only: bool,
) -> Changes:
    commits = (
        commit
        for commit in reversed(
            (
                await git("log", "--format=%H", f"{base_commit_sha}..{dest_branch}")
            ).split(
                "\n",
            ),
        )
        if commit
    )
    changes = Changes(stack_prefix)
    remaining_remote_changes = remote_changes.copy()

    for idx, commit in enumerate(commits):
        message = await git("log", "-1", "--format=%b", commit)
        title = await git("log", "-1", "--format=%s", commit)

        changeids = CHANGEID_RE.findall(message)
        if not changeids:
            console.print(
                f"`Change-Id:` line is missing on commit {commit}",
                style="red",
            )
            console.print(
                "Did you run `mergify stack --setup` for this repository?",
            )
            sys.exit(1)

        changeid = ChangeId(changeids[-1])
        pull = remaining_remote_changes.pop(changeid, None)

        action: ActionT
        if next_only and idx > 0:
            action = "skip-next-only"
        elif pull is None:
            if only_update_existing_pulls:
                action = "skip-create"
            action = "create"
        elif pull["merged_at"]:
            action = "skip-merged"
        elif pull["head"]["sha"] == commit:
            action = "skip-up-to-date"
        else:
            action = "update"

        changes.locals.append(
            LocalChange(
                changeid,
                pull,
                commit,
                title,
                message,
                changes.locals[-1].dest_branch if changes.locals else base_branch,
                f"{stack_prefix}/{changeid}",
                action,
            ),
        )

    for changeid, pull in remaining_remote_changes.items():
        changes.orphans.append(OrphanChange(changeid, pull))

    return changes


def get_log_from_local_change(
    change: LocalChange,
    dry_run: bool,
    create_as_draft: bool,
) -> str:
    url = f"<{change.dest_branch}>" if change.pull is None else change.pull["html_url"]

    flags: str = ""
    if change.pull and change.pull["draft"]:
        flags += " [yellow](draft)[/]"

    if change.action == "create":
        color = "yellow" if dry_run else "blue"
        action = "to create" if dry_run else "created"
        commit_info = change.commit_short_sha
        if create_as_draft:
            flags += " [yellow](draft)[/]"

    elif change.action == "update":
        color = "yellow" if dry_run else "blue"
        action = "to update" if dry_run else "updated"
        commit_info = f"{change.pull_short_head_sha} -> {change.commit_short_sha}"

    elif change.action == "skip-create":
        color = "grey"
        action = "skip, --only-update-existing-pulls"
        commit_info = change.commit_short_sha

    elif change.action == "skip-merged":
        color = "purple"
        action = "merged"
        flags += " [purple](merged)[/]"
        commit_info = (
            f"{change.pull['merge_commit_sha'][7:]}"
            if change.pull
            and change.pull["merged_at"]
            and change.pull["merge_commit_sha"]
            else change.commit_short_sha
        )

    elif change.action == "skip-next-only":
        color = "grey"
        action = "skip, --next-only"
        commit_info = change.commit_short_sha

    elif change.action == "skip-up-to-date":
        color = "grey"
        action = "up-to-date"
        commit_info = change.commit_short_sha

    else:
        # NOTE: we don't want to miss any action
        msg = f"Unhandled action: {change.action}"  # type: ignore[unreachable]
        raise RuntimeError(msg)

    return f"* [{color}]\\[{action}][/] '[red]{commit_info}[/] - [b]{change.title}[/]{flags} {url}"


def get_log_from_orphan_change(change: OrphanChange, dry_run: bool) -> str:
    action = "to delete" if dry_run else "deleted"
    title = change.pull["title"] if change.pull else "<unknown>"
    url = change.pull["html_url"] if change.pull else "<unknown>"
    sha = change.pull["head"]["sha"][7:] if change.pull else "<unknown>"
    return f"* [red]\\[{action}][/] '[red]{sha}[/] - [b]{title}[/] {url}"


def display_changes_plan(
    changes: Changes,
    create_as_draft: bool,
) -> None:
    for change in changes.locals:
        console.log(
            get_log_from_local_change(
                change,
                dry_run=True,
                create_as_draft=create_as_draft,
            ),
        )

    for orphan in changes.orphans:
        console.log(get_log_from_orphan_change(orphan, dry_run=True))


async def create_or_update_comments(
    client: httpx.AsyncClient,
    pulls: list[github_types.PullRequest],
) -> None:
    stack_comment = StackComment(pulls)

    for pull in pulls:
        if pull["merged_at"]:
            continue

        new_body = stack_comment.body(pull)

        r = await client.get(f"issues/{pull['number']}/comments")
        check_for_status(r)

        comments = typing.cast(list[github_types.Comment], r.json())
        for comment in comments:
            if StackComment.is_stack_comment(comment):
                if comment["body"] != new_body:
                    await client.patch(comment["url"], json={"body": new_body})
                break
        else:
            # NOTE(charly): dont't create a stack comment if there is only one
            # pull, it's not a stack
            if len(pulls) == 1:
                continue

            await client.post(
                f"issues/{pull['number']}/comments",
                json={"body": new_body},
            )


@dataclasses.dataclass
class StackComment:
    pulls: list[github_types.PullRequest]

    STACK_COMMENT_FIRST_LINE = "This pull request is part of a stack:\n"

    def body(self, current_pull: github_types.PullRequest) -> str:
        body = self.STACK_COMMENT_FIRST_LINE

        for pull in self.pulls:
            body += f"1. {pull['title']} ([#{pull['number']}]({pull['html_url']}))"
            if pull == current_pull:
                body += " 👈"
            body += "\n"

        return body

    @staticmethod
    def is_stack_comment(comment: github_types.Comment) -> bool:
        return comment["body"].startswith(StackComment.STACK_COMMENT_FIRST_LINE)


async def create_or_update_stack(  # noqa: PLR0913,PLR0917
    client: httpx.AsyncClient,
    remote: str,
    change: LocalChange,
    depends_on: github_types.PullRequest | None,
    create_as_draft: bool,
    keep_pull_request_title_and_body: bool,
) -> github_types.PullRequest:
    if change.pull is None:
        status_message = f"* creating stacked branch `{change.dest_branch}` ({change.commit_short_sha})"
    else:
        status_message = f"* updating stacked branch `{change.dest_branch}` ({change.commit_short_sha}) - {change.pull['html_url'] if change.pull else '<stack branch without associated pull>'})"

    with console.status(status_message):
        await git("branch", TMP_STACK_BRANCH, change.commit_sha)
        try:
            await git(
                "push",
                "-f",
                remote,
                TMP_STACK_BRANCH + ":" + change.dest_branch,
            )
        finally:
            await git("branch", "-D", TMP_STACK_BRANCH)

    if change.action == "update":
        if change.pull is None:
            msg = "Can't update pull with change.pull unset"
            raise RuntimeError(msg)

        with console.status(
            f"* updating pull request `{change.title}` (#{change.pull['number']}) ({change.commit_short_sha})",
        ):
            pull_changes = {
                "head": change.dest_branch,
                "base": change.base_branch,
            }
            if keep_pull_request_title_and_body:
                if change.pull["body"] is None:
                    msg = "GitHub returned a pull request without body set"
                    raise RuntimeError(msg)
                pull_changes.update(
                    {"body": format_pull_description(change.pull["body"], depends_on)},
                )
            else:
                pull_changes.update(
                    {
                        "title": change.title,
                        "body": format_pull_description(change.message, depends_on),
                    },
                )

            r = await client.patch(f"pulls/{change.pull['number']}", json=pull_changes)
            check_for_status(r)
            return change.pull

    elif change.action == "create":
        with console.status(
            f"* creating stacked pull request `{change.title}` ({change.commit_short_sha})",
        ):
            r = await client.post(
                "pulls",
                json={
                    "title": change.title,
                    "body": format_pull_description(change.message, depends_on),
                    "draft": create_as_draft,
                    "head": change.dest_branch,
                    "base": change.base_branch,
                },
            )
            check_for_status(r)
            return typing.cast(github_types.PullRequest, r.json())

    msg = f"Unhandled action: {change.action}"
    raise RuntimeError(msg)


async def delete_stack(
    client: httpx.AsyncClient,
    stack_prefix: str,
    change: OrphanChange,
) -> None:
    r = await client.delete(f"git/refs/heads/{stack_prefix}/{change.id}")
    check_for_status(r)
    console.log(get_log_from_orphan_change(change, dry_run=False))


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


async def git_get_branch_name() -> str:
    return await git("rev-parse", "--abbrev-ref", "HEAD")


async def git_get_target_branch(branch: str) -> str:
    return (await git("config", "--get", "branch." + branch + ".merge")).removeprefix(
        "refs/heads/",
    )


async def git_get_target_remote(branch: str) -> str:
    return await git("config", "--get", "branch." + branch + ".remote")


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


def trunk_type(trunk: str) -> tuple[str, str]:
    result = trunk.split("/", maxsplit=1)
    if len(result) != 2:
        msg = "Trunk is invalid. It must be origin/branch-name [/]"
        raise argparse.ArgumentTypeError(msg)
    return result[0], result[1]


@dataclasses.dataclass
class LocalBranchInvalidError(Exception):
    message: str


def check_local_branch(branch_name: str, branch_prefix: str) -> None:
    if branch_name.startswith(branch_prefix) and re.search(
        r"I[0-9a-z]{40}$",
        branch_name,
    ):
        msg = "Local branch is a branch generated by Mergify CLI"
        raise LocalBranchInvalidError(msg)


async def stack_edit(_: argparse.Namespace) -> None:
    os.chdir(await git("rev-parse", "--show-toplevel"))
    trunk = await get_trunk()
    base = await git("merge-base", trunk, "HEAD")
    os.execvp("git", ("git", "rebase", "-i", f"{base}^"))  # noqa: S606


async def get_remote_changes(
    client: httpx.AsyncClient,
    user: str,
    stack_prefix: str,
) -> RemoteChanges:
    known_changeids = RemoteChanges({})
    r = await client.get(f"git/matching-refs/heads/{stack_prefix}/")
    check_for_status(r)
    refs = typing.cast(list[github_types.GitRef], r.json())

    tasks = [
        asyncio.create_task(
            get_change_id_and_pull_from_github(client, user, stack_prefix, ref),
        )
        for ref in refs
        # For backward compat
        if not ref["ref"].endswith("/aio")
    ]
    if tasks:
        done, _ = await asyncio.wait(tasks)
        for task in done:
            known_changeids.update(dict([await task]))
    return known_changeids


# TODO(charly): fix code to conform to linter (number of arguments, local
# variables, statements, positional arguments, branches)
async def stack_push(  # noqa: PLR0913, PLR0915, PLR0917, PLR0912
    github_server: str,
    token: str,
    skip_rebase: bool,
    next_only: bool,
    branch_prefix: str,
    dry_run: bool,
    trunk: tuple[str, str],
    create_as_draft: bool = False,
    keep_pull_request_title_and_body: bool = False,
    only_update_existing_pulls: bool = False,
) -> None:
    os.chdir(await git("rev-parse", "--show-toplevel"))
    dest_branch = await git_get_branch_name()

    try:
        check_local_branch(branch_name=dest_branch, branch_prefix=branch_prefix)
    except LocalBranchInvalidError as e:
        console.log(f"[red] {e.message} [/]")
        console.log(
            "You should run `mergify stack` on the branch you created in the first place",
        )
        sys.exit(1)

    remote, base_branch = trunk

    user, repo = get_slug(await git("config", "--get", f"remote.{remote}.url"))

    if base_branch == dest_branch:
        console.log("[red] base branch and destination branch are the same [/]")
        sys.exit(1)

    stack_prefix = f"{branch_prefix}/{dest_branch}"

    if not dry_run:
        if skip_rebase:
            console.log(f"branch `{dest_branch}` rebase skipped (--skip-rebase)")
        else:
            with console.status(
                f"Rebasing branch `{dest_branch}` on `{remote}/{base_branch}`...",
            ):
                await git("pull", "--rebase", remote, base_branch)
            console.log(f"branch `{dest_branch}` rebased on `{remote}/{base_branch}`")

    base_commit_sha = await git("merge-base", "--fork-point", f"{remote}/{base_branch}")
    if not base_commit_sha:
        console.log(
            f"Common commit between `{remote}/{base_branch}` and `{dest_branch}` branches not found",
            style="red",
        )
        sys.exit(1)

    if DEBUG:
        event_hooks = {"request": [log_httpx_request], "response": [log_httpx_response]}
    else:
        event_hooks = {}

    async with httpx.AsyncClient(
        base_url=f"{github_server}/repos/{user}/{repo}/",
        headers={
            "Accept": "application/vnd.github.v3+json",
            "User-Agent": f"mergify_cli/{VERSION}",
            "Authorization": f"token {token}",
        },
        event_hooks=event_hooks,  # type: ignore[arg-type]
        follow_redirects=True,
        timeout=5.0,
    ) as client:
        with console.status("Retrieving latest pushed stacks"):
            remote_changes = await get_remote_changes(client, user, stack_prefix)

        with console.status("Preparing stacked branches..."):
            console.log("Stacked pull request plan:", style="green")
            changes = await get_changes(
                base_commit_sha,
                stack_prefix,
                base_branch,
                dest_branch,
                remote_changes,
                only_update_existing_pulls,
                next_only,
            )

        display_changes_plan(
            changes,
            create_as_draft,
        )

        if dry_run:
            console.log("[orange]Finished (dry-run mode) :tada:[/]")
            sys.exit(0)

        console.log("Updating and/or creating stacked pull requests:", style="green")

        pulls_to_comment: list[github_types.PullRequest] = []
        for change in changes.locals:
            depends_on = pulls_to_comment[-1] if pulls_to_comment else None

            if change.action in {"create", "update"}:
                pull = await create_or_update_stack(
                    client,
                    remote,
                    change,
                    depends_on,
                    create_as_draft,
                    keep_pull_request_title_and_body,
                )
                change.pull = pull

            if change.pull:
                pulls_to_comment.append(change.pull)

            console.log(
                get_log_from_local_change(
                    change,
                    dry_run=False,
                    create_as_draft=create_as_draft,
                ),
            )

        with console.status("Updating comments..."):
            await create_or_update_comments(client, pulls_to_comment)

        console.log("[green]Comments updated")

        with console.status("Deleting unused branches..."):
            if changes.orphans:
                await asyncio.wait(
                    asyncio.create_task(
                        delete_stack(client, stack_prefix, change),
                    )
                    for change in changes.orphans
                )

        console.log("[green]Finished :tada:[/]")


def format_pull_description(
    message: str,
    depends_on: github_types.PullRequest | None,
) -> str:
    depends_on_header = ""
    if depends_on is not None:
        depends_on_header = f"\n\nDepends-On: #{depends_on['number']}"

    message = CHANGEID_RE.sub("", message).rstrip("\n")
    message = DEPENDS_ON_RE.sub("", message).rstrip("\n")

    return message + depends_on_header


def GitHubToken(v: str) -> str:  # noqa: N802
    if not v:
        raise ValueError
    return v


async def get_default_github_server() -> str:
    try:
        result = await git("config", "--get", "mergify-cli.github-server")
    except CommandError:
        result = ""

    url = parse.urlparse(result or "https://api.github.com/")
    url = url._replace(scheme="https")

    if url.hostname == "api.github.com":
        url = url._replace(path="")
    else:
        url = url._replace(path="/api/v3")
    return url.geturl()


async def get_default_branch_prefix() -> str:
    try:
        result = await git("config", "--get", "mergify-cli.stack-branch-prefix")
    except CommandError:
        result = ""

    return result or "mergify_cli"


async def get_default_keep_pr_title_body() -> bool:
    try:
        result = await git("config", "--get", "mergify-cli.stack-keep-pr-title-body")
    except CommandError:
        return False

    return result == "true"


async def get_default_token() -> str:
    token = os.environ.get("GITHUB_TOKEN", "")
    if not token:
        try:
            token = await _run_command("gh", "auth", "token")
        except CommandError:
            console.print(
                "error: please make sure that gh client is installed and you are authenticated, or set the "
                "'GITHUB_TOKEN' environment variable",
            )
    if DEBUG:
        console.print(f"[purple]DEBUG: token: {token}[/]")
    return token


async def _stack_push(args: argparse.Namespace) -> None:
    if args.setup:
        # backward compat
        await stack_setup(args)
        return

    await stack_push(
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
    )


def register_stack_setup_parser(
    sub_parsers: argparse._SubParsersAction[typing.Any],
) -> None:
    parser = sub_parsers.add_parser(
        "setup",
        description="Configure the git hooks",
        help="Initial installation of the required git commit-msg hook",
    )
    parser.set_defaults(func=stack_setup)


def register_stack_edit_parser(
    sub_parsers: argparse._SubParsersAction[typing.Any],
) -> None:
    parser = sub_parsers.add_parser(
        "edit",
        description="Edit the stack history",
        help="Edit the stack history",
    )
    parser.set_defaults(func=stack_edit)


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
        "--trunk",
        "-t",
        type=trunk_type,
        default=await get_trunk(),
        help="Change the target branch of the stack.",
    )
    parser.add_argument(
        "--branch-prefix",
        default=await get_default_branch_prefix(),
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
    register_stack_edit_parser(stack_sub_parsers)
    register_stack_setup_parser(stack_sub_parsers)

    known_args, _ = parser.parse_known_args(args)

    # Default
    if known_args.action is None:
        args.insert(0, "stack")

    known_args, _ = parser.parse_known_args(args)

    if known_args.action == "stack" and known_args.stack_action is None:
        args.insert(1, "push")

    return parser.parse_args(args)


async def main() -> None:
    args = await parse_args(sys.argv[1:])

    if args.debug:
        global DEBUG  # noqa: PLW0603
        DEBUG = True

    await args.func(args)


def cli() -> None:
    asyncio.run(main())
