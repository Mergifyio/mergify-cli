from __future__ import annotations

import dataclasses
import sys
from typing import TYPE_CHECKING

from mergify_cli import console
from mergify_cli import console_error
from mergify_cli import utils
from mergify_cli.exit_codes import ExitCode
from mergify_cli.stack import changes


if TYPE_CHECKING:
    from mergify_cli import github_types


@dataclasses.dataclass
class ChangeNode:
    pull: github_types.PullRequest
    up: ChangeNode | None = None


async def stack_checkout(
    github_server: str,
    token: str,
    *,
    user: str,
    repo: str,
    branch_prefix: str | None,
    name: str,
    branch: str | None,
    author: str | None,
    trunk: tuple[str, str],
    dry_run: bool,
) -> None:
    if author is None:
        async with utils.get_github_http_client(github_server, token) as client:
            r_author = await client.get("/user")
            author = r_author.json()["login"]

    if branch_prefix is None:
        branch_prefix = await utils.get_default_branch_prefix(author)

    # Strip change ID suffix if present (e.g. /Ibb431d523fb75f48f387a3964d2936ada933cffe)
    name = changes.CHANGEID_SUFFIX_RE.sub("", name)

    # Strip branch prefix from name if already included
    if branch_prefix and name.startswith(f"{branch_prefix}/"):
        name = name.removeprefix(f"{branch_prefix}/")

    # Local branch name defaults to the remote stack name
    if branch is None:
        branch = name

    stack_branch = f"{branch_prefix}/{name}" if branch_prefix else name

    async with utils.get_github_http_client(github_server, token) as client:
        with console.status("Retrieving latest pushed stacks"):
            remote_changes = await changes.get_remote_changes(
                client,
                user,
                repo,
                stack_branch,
                author,
            )

        root_node: ChangeNode | None = None

        nodes = {
            pull["base"]["ref"]: ChangeNode(pull)
            for pull in remote_changes.values()
            if pull["state"] == "open"
        }

        # Linking nodes and finding the base
        for node in nodes.values():
            node.up = nodes.get(node.pull["head"]["ref"])

            if not node.pull["base"]["ref"].startswith(stack_branch):
                if root_node is not None:
                    console_error(
                        "unexpected stack layout, two root commits found",
                    )
                    sys.exit(ExitCode.INVALID_STATE)
                root_node = node

        if root_node is None:
            console.print("No stacked pull requests found")
            sys.exit(0)

        console.log("Stacked pull requests:")
        node = root_node
        while True:
            pull = node.pull
            console.log(
                f"* [b][white]#{pull['number']}[/] {pull['title']}[/]  {pull['html_url']}",
            )
            console.log(f"  [grey42]{pull['base']['ref']} -> {pull['head']['ref']}[/]")

            if node.up is None:
                break
            node = node.up

        if dry_run:
            return

        remote = trunk[0]
        upstream = f"{remote}/{root_node.pull['base']['ref']}"
        head_ref = f"{remote}/{node.pull['head']['ref']}"
        await utils.git("fetch", remote, node.pull["head"]["ref"])
        await utils.git("checkout", "-b", branch, head_ref)
        await utils.git("branch", f"--set-upstream-to={upstream}")
