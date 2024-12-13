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
import os
import re
import sys
import typing

from mergify_cli import console
from mergify_cli import github_types
from mergify_cli import utils
from mergify_cli.stack import changes


if typing.TYPE_CHECKING:
    import httpx

DEPENDS_ON_RE = re.compile(r"Depends-On: (#[0-9]*)")
TMP_STACK_BRANCH = "mergify-cli-tmp"


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


def format_pull_description(
    message: str,
    depends_on: github_types.PullRequest | None,
) -> str:
    depends_on_header = ""
    if depends_on is not None:
        depends_on_header = f"\n\nDepends-On: #{depends_on['number']}"

    message = changes.CHANGEID_RE.sub("", message).rstrip("\n")
    message = DEPENDS_ON_RE.sub("", message).rstrip("\n")

    return message + depends_on_header


# TODO(charly): fix code to conform to linter (number of arguments, local
# variables, statements, positional arguments, branches)
async def stack_push(  # noqa: PLR0912, PLR0913, PLR0915, PLR0917, PLR0914
    github_server: str,
    token: str,
    skip_rebase: bool,
    next_only: bool,
    branch_prefix: str | None,
    dry_run: bool,
    trunk: tuple[str, str],
    create_as_draft: bool = False,
    keep_pull_request_title_and_body: bool = False,
    only_update_existing_pulls: bool = False,
    author: str | None = None,
) -> None:
    os.chdir(await utils.git("rev-parse", "--show-toplevel"))
    dest_branch = await utils.git_get_branch_name()

    if author is None:
        async with utils.get_github_http_client(github_server, token) as client:
            r_author = await client.get("/user")
            author = r_author.json()["login"]

    if branch_prefix is None:
        branch_prefix = await utils.get_default_branch_prefix(author)

    try:
        check_local_branch(branch_name=dest_branch, branch_prefix=branch_prefix)
    except LocalBranchInvalidError as e:
        console.log(f"[red] {e.message} [/]")
        console.log(
            "You should run `mergify stack` on the branch you created in the first place",
        )
        sys.exit(1)

    remote, base_branch = trunk

    user, repo = utils.get_slug(
        await utils.git("config", "--get", f"remote.{remote}.url"),
    )

    if base_branch == dest_branch:
        remote_url = await utils.git("remote", "get-url", remote)
        console.print(
            f"Your local branch `{dest_branch}` targets itself: `{remote}/{base_branch}` (at {remote_url}@{base_branch}).\n"
            f"You should either fix the target branch or rename your local branch.\n\n"
            f"* To fix the target branch: `git branch {dest_branch} --set-upstream-to={remote}/main>\n",
            f"* To rename your local branch: `git branch -M {dest_branch} new-branch-name`",
            style="red",
        )
        sys.exit(1)

    stack_prefix = f"{branch_prefix}/{dest_branch}" if branch_prefix else dest_branch

    if not dry_run:
        if skip_rebase:
            console.log(f"branch `{dest_branch}` rebase skipped (--skip-rebase)")
        else:
            with console.status(
                f"Rebasing branch `{dest_branch}` on `{remote}/{base_branch}`...",
            ):
                await utils.git("pull", "--rebase", remote, base_branch)
            console.log(f"branch `{dest_branch}` rebased on `{remote}/{base_branch}`")

    base_commit_sha = await utils.git(
        "merge-base",
        "--fork-point",
        f"{remote}/{base_branch}",
    )
    if not base_commit_sha:
        console.log(
            f"Common commit between `{remote}/{base_branch}` and `{dest_branch}` branches not found",
            style="red",
        )
        sys.exit(1)

    async with utils.get_github_http_client(github_server, token) as client:
        with console.status("Retrieving latest pushed stacks"):
            remote_changes = await changes.get_remote_changes(
                client,
                user,
                repo,
                stack_prefix,
                author,
            )

        with console.status("Preparing stacked branches..."):
            console.log("Stacked pull request plan:", style="green")
            planned_changes = await changes.get_changes(
                base_commit_sha,
                stack_prefix,
                base_branch,
                dest_branch,
                remote_changes,
                only_update_existing_pulls,
                next_only,
            )

        changes.display_plan(
            planned_changes,
            create_as_draft,
        )

        if dry_run:
            console.log("[orange]Finished (dry-run mode) :tada:[/]")
            sys.exit(0)

        console.log("Updating and/or creating stacked pull requests:", style="green")

        pulls_to_comment: list[github_types.PullRequest] = []
        for change in planned_changes.locals:
            depends_on = pulls_to_comment[-1] if pulls_to_comment else None

            if change.action in {"create", "update"}:
                pull = await create_or_update_stack(
                    client,
                    user,
                    repo,
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
                change.get_log_from_local_change(
                    dry_run=False,
                    create_as_draft=create_as_draft,
                ),
            )

        with console.status("Updating comments..."):
            await create_or_update_comments(client, user, repo, pulls_to_comment)

        console.log("[green]Comments updated")

        with console.status("Deleting unused branches..."):
            if planned_changes.orphans:
                await asyncio.wait(
                    asyncio.create_task(
                        delete_stack(client, user, repo, stack_prefix, change),
                    )
                    for change in planned_changes.orphans
                )

        console.log("[green]Finished :tada:[/]")


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


async def create_or_update_comments(
    client: httpx.AsyncClient,
    user: str,
    repo: str,
    pulls: list[github_types.PullRequest],
) -> None:
    stack_comment = StackComment(pulls)

    for pull in pulls:
        if pull["merged_at"]:
            continue

        new_body = stack_comment.body(pull)

        r = await client.get(f"/repos/{user}/{repo}/issues/{pull['number']}/comments")
        comments = typing.cast("list[github_types.Comment]", r.json())
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
                f"/repos/{user}/{repo}/issues/{pull['number']}/comments",
                json={"body": new_body},
            )


async def delete_stack(
    client: httpx.AsyncClient,
    user: str,
    repo: str,
    stack_prefix: str,
    change: changes.OrphanChange,
) -> None:
    await client.delete(
        f"/repos/{user}/{repo}/git/refs/heads/{stack_prefix}/{change.id}",
    )
    console.log(change.get_log_from_orphan_change(dry_run=False))


async def create_or_update_stack(  # noqa: PLR0913,PLR0917
    client: httpx.AsyncClient,
    user: str,
    repo: str,
    remote: str,
    change: changes.LocalChange,
    depends_on: github_types.PullRequest | None,
    create_as_draft: bool,
    keep_pull_request_title_and_body: bool,
) -> github_types.PullRequest:
    if change.pull is None:
        status_message = f"* creating stacked branch `{change.dest_branch}` ({change.commit_short_sha})"
    else:
        status_message = f"* updating stacked branch `{change.dest_branch}` ({change.commit_short_sha}) - {change.pull['html_url'] if change.pull else '<stack branch without associated pull>'})"

    with console.status(status_message):
        await utils.git("branch", TMP_STACK_BRANCH, change.commit_sha)
        try:
            await utils.git(
                "push",
                "-f",
                remote,
                TMP_STACK_BRANCH + ":" + change.dest_branch,
            )
        finally:
            await utils.git("branch", "-D", TMP_STACK_BRANCH)

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
                pull_changes.update(
                    {
                        "body": format_pull_description(
                            change.pull["body"] or "",
                            depends_on,
                        ),
                    },
                )
            else:
                pull_changes.update(
                    {
                        "title": change.title,
                        "body": format_pull_description(change.message, depends_on),
                    },
                )

            r = await client.patch(
                f"/repos/{user}/{repo}/pulls/{change.pull['number']}",
                json=pull_changes,
            )
            return change.pull

    elif change.action == "create":
        with console.status(
            f"* creating stacked pull request `{change.title}` ({change.commit_short_sha})",
        ):
            r = await client.post(
                f"/repos/{user}/{repo}/pulls",
                json={
                    "title": change.title,
                    "body": format_pull_description(change.message, depends_on),
                    "draft": create_as_draft,
                    "head": change.dest_branch,
                    "base": change.base_branch,
                },
            )
            return typing.cast("github_types.PullRequest", r.json())

    msg = f"Unhandled action: {change.action}"
    raise RuntimeError(msg)
