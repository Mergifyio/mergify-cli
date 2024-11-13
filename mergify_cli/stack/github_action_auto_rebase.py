from __future__ import annotations

import json
import os
import sys

import aiofiles

from mergify_cli import console
from mergify_cli import utils
from mergify_cli.stack import checkout
from mergify_cli.stack import push


async def stack_github_action_auto_rebase(github_server: str, token: str) -> None:
    for env in ("GITHUB_EVENT_NAME", "GITHUB_EVENT_PATH", "GITHUB_REPOSITORY"):
        if env not in os.environ:
            console.log("This action only works in a GitHub Action", style="red")
            sys.exit(1)

    event_name = os.environ["GITHUB_EVENT_NAME"]
    event_path = os.environ["GITHUB_EVENT_PATH"]
    user, repo = os.environ["GITHUB_REPOSITORY"].split("/")

    async with aiofiles.open(event_path) as f:
        event = json.loads(await f.read())

    if event_name != "issue_comment" or not event["issue"]["pull_request"]:
        console.log(
            "This action only works with `issue_comment` event for pull request",
            style="red",
        )
        sys.exit(1)

    async with utils.get_github_http_client(github_server, token) as client:
        await client.post(
            f"/repos/{user}/{repo}/issues/comments/{event['comment']['id']}/reactions",
            json={"content": "+1"},
        )
        resp = await client.get(event["issue"]["pull_request"]["url"])
        pull = resp.json()

    author = pull["user"]["login"]
    base = pull["base"]["ref"]
    head = pull["head"]["ref"]

    head_changeid = head.split("/")[-1]
    if not head_changeid.startswith("I") or len(head_changeid) != 41:
        console.log("This pull request is not part of a stack", style="red")
        sys.exit(1)

    base_changeid = base.split("/")[-1]
    if base_changeid.startswith("I") and len(base_changeid) == 41:
        console.log("This pull request is not the bottom of the stack", style="red")
        sys.exit(1)

    stack_branch = head.removesuffix(f"/{head_changeid}")

    await utils.git("config", "--global", "user.name", f"{author}")
    await utils.git(
        "config",
        "--global",
        "user.email",
        f"{author}@users.noreply.github.com",
    )
    await utils.git("branch", "--set-upstream-to", f"origin/{base}")

    await checkout.stack_checkout(
        github_server,
        token,
        user=user,
        repo=repo,
        branch_prefix="",
        branch=stack_branch,
        author=author,
        trunk=("origin", base),
        dry_run=False,
    )
    await push.stack_push(
        github_server,
        token,
        skip_rebase=False,
        next_only=False,
        branch_prefix="",
        dry_run=False,
        trunk=("origin", base),
        create_as_draft=False,
        keep_pull_request_title_and_body=True,
        only_update_existing_pulls=False,
        author=author,
    )

    async with utils.get_github_http_client(github_server, token) as client:
        body_quote = "> " + "\n> ".join(event["comment"]["body"].split("\n"))
        await client.post(
            f"/repos/{user}/{repo}/issues/{pull['number']}/comments",
            json={"body": f"{body_quote}\n\nThe stack has been rebased"},
        )
