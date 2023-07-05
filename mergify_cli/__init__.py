#
#  Copyright © 2021 Mergify SAS
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


import argparse
import asyncio
import importlib.metadata
import os
import re
import subprocess
import sys
import typing
from urllib import parse

import httpx
import rich
import rich.console

from mergify_cli.commit_msg_hook import COMMIT_MSG_HOOK


try:
    VERSION = importlib.metadata.version("mrgfy")
except ImportError:
    # https://pyoxidizer.readthedocs.io/en/stable/oxidized_importer_behavior_and_compliance.html#importlib-metadata-compatibility
    VERSION = "0.1"

CHANGEID_RE = re.compile(r"Change-Id: (I[0-9a-z]{40})")
READY_FOR_REVIEW_TEMPLATE = 'mutation { markPullRequestReadyForReview(input: { pullRequestId: "%s" }) { clientMutationId } }'
console = rich.console.Console(log_path=False, log_time=False)

DEBUG = False


def check_for_graphql_errors(response: httpx.Response) -> None:
    data = response.json()
    if "errors" in data:
        console.print(f"url: {response.request.url}", style="red")
        console.print(f"data: {response.request.content.decode()}", style="red")
        if "errors" in data:
            console.print(
                "\n".join(f"* {e.get('message') or e}" for e in data["errors"]),
                style="red",
            )
        sys.exit(1)


def check_for_status(response: httpx.Response) -> None:
    if response.status_code < 400:
        return

    if response.status_code < 500:
        data = response.json()
        console.print(f"url: {response.request.url}", style="red")
        console.print(f"data: {response.request.content.decode()}", style="red")
        console.print(
            f"HTTPError {response.status_code}: {data['message']}", style="red"
        )
        if "errors" in data:
            console.print(
                "\n".join(f"* {e.get('message') or e}" for e in data["errors"]),
                style="red",
            )
        sys.exit(1)

    response.raise_for_status()


async def git(args: str) -> str:
    if DEBUG:
        console.print(f"[purple]DEBUG: running: git {args} [/]")
    proc = await asyncio.create_subprocess_shell(
        f"git {args}",
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.STDOUT,
    )
    stdout, _ = await proc.communicate()
    if proc.returncode != 0:
        console.log(f"fail to run `git {args}`:", style="red")
        console.log(f"{stdout.decode()}", style="red")
        sys.exit(1)
    return stdout.decode().strip()


def get_slug(url: str) -> tuple[str, str]:
    parsed = parse.urlparse(url)
    if parsed.netloc == "":
        # Probably ssh
        _, _, path = parsed.path.partition(":")
    else:
        path = parsed.path[1:].rstrip("/")

    user, repo = path.split("/", 1)
    if repo.endswith(".git"):
        repo = repo[:-4]
    return user, repo


async def do_setup() -> None:
    hooks_dir = (await git("rev-parse --git-path hooks")).strip()
    hook_file = os.path.join(hooks_dir, "commit-msg")
    if os.path.exists(hook_file):
        with open(hook_file) as f:
            data = f.read()
        if data != COMMIT_MSG_HOOK:
            console.print(
                f"error: {hook_file} differ from mergify_cli hook", style="red"
            )
            sys.exit(1)

    else:
        console.log("Installation of git commit-msg hook")
        with open(hook_file, "w") as f:
            f.write(COMMIT_MSG_HOOK)
        os.chmod(hook_file, 0o755)


class GitRef(typing.TypedDict):
    ref: str


class HeadRef(typing.TypedDict):
    sha: str


class PullRequest(typing.TypedDict):
    html_url: str
    number: str
    title: str
    head: HeadRef
    state: str
    draft: bool
    node_id: str


ChangeId = typing.NewType("ChangeId", str)
KnownChangeIDs = typing.NewType("KnownChangeIDs", dict[ChangeId, PullRequest | None])


async def get_changeid_and_pull(
    client: httpx.AsyncClient, user: str, stack_prefix: str, ref: GitRef
) -> tuple[ChangeId, PullRequest | None]:
    branch = ref["ref"][len("refs/heads/") :]
    changeid = ChangeId(branch[len(stack_prefix) + 1 :])
    r = await client.get("pulls", params={"head": f"{user}:{branch}", "state": "open"})
    check_for_status(r)
    pulls = [
        p for p in typing.cast(list[PullRequest], r.json()) if p["state"] == "open"
    ]
    if len(pulls) > 1:
        raise RuntimeError(f"More than 1 pull found with this head: {branch}")
    if pulls:
        return changeid, pulls[0]
    return changeid, None


Change = typing.NewType("Change", tuple[ChangeId, str, str, str])


async def get_local_changes(
    commits: list[str],
    stack_prefix: str,
    known_changeids: KnownChangeIDs,
) -> list[Change]:
    changes = []
    for i, commit in enumerate(commits):
        message = await git(f"log -1 --format='%b' {commit}")
        title = await git(f"log -1 --format='%s' {commit}")
        changeids = CHANGEID_RE.findall(message)
        if not changeids:
            console.print(
                f"`Change-Id:` line is missing on commit {commit}", style="red"
            )
            sys.exit(1)
        changeid = ChangeId(changeids[-1])
        changes.append(Change((changeid, commit, title, message)))
        pull = known_changeids.get(changeid)
        if pull is None:
            action = "to create"
            url = f"<{stack_prefix}/{changeid}>"
            commit_info = commit[-7:]
        elif commit == pull["head"]["sha"]:
            if i == 0 and pull["draft"]:
                action = "ready_for_review"
            else:
                action = "nothing"
            url = pull["html_url"]
            commit_info = commit[-7:]
        else:
            action = "to update"
            url = pull["html_url"]
            commit_info = f"{pull['head']['sha'][-7:]} -> {commit[-7:]}"
        console.log(
            f"* [yellow]\\[{action}][/] '[red]{commit_info}[/] - [b]{title}[/] {url} - {changeid}"
        )

    return changes


async def get_changeids_to_delete(
    changes: list[Change], known_changeids: KnownChangeIDs
) -> set[ChangeId]:
    changeids_to_delete = set(known_changeids.keys()) - {
        changeid for changeid, commit, title, message in changes
    }
    for changeid in changeids_to_delete:
        pull = known_changeids.get(changeid)
        if pull:
            console.log(
                f"* [red]\\[to delete][/] '[red]{pull['head']['sha'][-7:]}[/] - [b]{pull['title']}[/] {pull['html_url']} - {changeid}"
            )
        else:
            console.log(
                f"* [red]\\[to delete][/] '[red].......[/] - [b]<missing pull request>[/] - {changeid}"
            )
    return changeids_to_delete


async def create_or_update_comments(
    client: httpx.AsyncClient, pulls: list[PullRequest]
) -> None:
    first_line = "This pull request is part of a stack:\n"
    body = first_line
    for pull in pulls:
        body += f"1. {pull['title']} ([#{pull['number']}]({pull['html_url']}))\n"

    for pull in pulls:
        r = await client.get(f"issues/{pull['number']}/comments")
        check_for_status(r)
        for comment in r.json():
            if comment["body"].startswith(first_line):
                if comment["body"] != body:
                    await client.patch(comment["url"], json={"body": body})
                break
        else:
            await client.post(f"issues/{pull['number']}/comments", json={"body": body})


async def create_or_update_stack(
    client: httpx.AsyncClient,
    stacked_base_branch: str,
    stacked_dest_branch: str,
    changeid: ChangeId,
    commit: str,
    title: str,
    message: str,
    ready_for_review: bool,
    known_changeids: KnownChangeIDs,
) -> tuple[PullRequest, str]:
    if changeid in known_changeids:
        pull = known_changeids.get(changeid)
        with console.status(
            f"* updating stacked branch `{stacked_dest_branch}` ({commit[-7:]}) - {pull['html_url'] if pull else '<stack branch without associated pull>'})"
        ):
            r = await client.patch(
                f"git/refs/heads/{stacked_dest_branch}",
                json={"sha": commit, "force": True},
            )
    else:
        with console.status(
            f"* creating stacked branch `{stacked_dest_branch}` ({commit[-7:]})"
        ):
            r = await client.post(
                "git/refs",
                json={"ref": f"refs/heads/{stacked_dest_branch}", "sha": commit},
            )

    check_for_status(r)

    pull = known_changeids.get(changeid)
    if pull and pull["head"]["sha"] == commit:
        if ready_for_review and pull["draft"]:
            action = "ready_for_review"
            r = await client.post(
                "https://api.github.com/graphql",
                headers={
                    "Accept": "application/vnd.github.v4.idl",
                    "User-Agent": f"mergify_cli/{VERSION}",
                    "Authorization": client.headers["Authorization"],
                },
                json={"query": READY_FOR_REVIEW_TEMPLATE % pull["node_id"]},
            )
            check_for_status(r)
            check_for_graphql_errors(r)
        else:
            action = "nothing"
    elif pull:
        action = "updated"
        with console.status(
            f"* updating pull request `{title}` (#{pull['number']}) ({commit[-7:]})"
        ):
            r = await client.patch(
                f"pulls/{pull['number']}",
                json={
                    "title": title,
                    "body": message,
                    "head": stacked_dest_branch,
                    "base": stacked_base_branch,
                },
            )
            check_for_status(r)
            pull = typing.cast(PullRequest, r.json())
            if ready_for_review and pull["draft"]:
                r = await client.post(
                    "https://api.github.com/graphql",
                    headers={
                        "Accept": "application/vnd.github.v4.idl",
                        "User-Agent": f"mergify_cli/{VERSION}",
                        "Authorization": client.headers["Authorization"],
                    },
                    json={"query": READY_FOR_REVIEW_TEMPLATE % pull["node_id"]},
                )
                check_for_status(r)
                check_for_graphql_errors(r)
    else:
        action = "created"
        with console.status(
            f"* creating stacked pull request `{title}` ({commit[-7:]})"
        ):
            r = await client.post(
                "pulls",
                json={
                    "title": title,
                    "body": message,
                    "draft": not ready_for_review,
                    "head": stacked_dest_branch,
                    "base": stacked_base_branch,
                },
            )
            check_for_status(r)
            pull = typing.cast(PullRequest, r.json())
    return pull, action


async def delete_stack(
    client: httpx.AsyncClient,
    stack_prefix: str,
    changeid: ChangeId,
    known_changeids: KnownChangeIDs,
) -> None:
    r = await client.delete(
        f"git/refs/heads/{stack_prefix}/{changeid}",
    )
    check_for_status(r)
    pull = known_changeids[changeid]
    if pull:
        console.log(
            f"* [red]\\[deleted][/] '[red]{pull['head']['sha'][-7:]}[/] - [b]{pull['title']}[/] {pull['html_url']} - {changeid}"
        )
    else:
        console.log(
            f"* [red]\\[deleted][/] '[red].......[/] - [b]<branch {stack_prefix}/{changeid}>[/] - {changeid}"
        )


async def log_httpx_request(request: httpx.Request) -> None:
    console.print(
        f"[purple]DEBUG: request: {request.method} {request.url} - Waiting for response[/]"
    )


async def log_httpx_response(response: httpx.Response) -> None:
    request = response.request
    console.print(
        f"[purple]DEBUG: response: {request.method} {request.url} - Status {response.status_code}[/]"
    )


async def main(
    token: str, stack: bool, next_only: bool, branch_prefix: str, dry_run: bool
) -> None:
    os.chdir((await git("rev-parse --show-toplevel")).strip())
    dest_branch = await git("rev-parse --abbrev-ref HEAD")
    remote, _, base_branch = (
        await git(f"for-each-ref --format='%(upstream:short)' refs/heads/{dest_branch}")
    ).partition("/")
    user, repo = get_slug(await git(f"config --get remote.{remote}.url"))

    if base_branch == dest_branch:
        console.log("[red] base branch and destination branch are the same [/]")
        sys.exit(1)

    stack_prefix = f"{branch_prefix}/{dest_branch}"

    if not dry_run:
        with console.status(
            f"Rebasing branch `{dest_branch}` on `{remote}/{base_branch}`...",
        ):
            await git(f"pull --rebase {remote} {base_branch}")
        console.log(f"branch `{dest_branch}` rebased on `{remote}/{base_branch}`")

        with console.status(
            f"Pushing branch `{dest_branch}` to `{remote}/{dest_branch}`...",
        ):
            await git(f"push -f {remote} {dest_branch}:{stack_prefix}/aio")
        console.log(f"branch `{dest_branch}` pushed to `{remote}/{dest_branch}` ")

    base_commit_sha = await git(f"merge-base --fork-point {remote}/{base_branch}")
    if not base_commit_sha:
        console.log(
            f"Common commit between `{remote}/{base_branch}` and `{dest_branch}` branches not found",
            style="red",
        )
        sys.exit(1)

    commits = [
        commit
        for commit in reversed(
            (await git(f"log --format='%H' {base_commit_sha}..{dest_branch}")).split(
                "\n"
            )
        )
        if commit.strip()
    ]

    if len(commits) > 1 and not stack:
        console.log("[red] too many commits and stack mode disabled [/]")
        sys.exit(1)

    known_changeids = KnownChangeIDs({})

    if DEBUG:
        event_hooks = {"request": [log_httpx_request], "response": [log_httpx_response]}
    else:
        event_hooks = {}

    async with httpx.AsyncClient(
        base_url=f"https://api.github.com/repos/{user}/{repo}/",
        headers={
            "Accept": "application/vnd.github.v3+json",
            "User-Agent": f"mergify_cli/{VERSION}",
            "Authorization": f"token {token}",
        },
        event_hooks=event_hooks,  # type: ignore[arg-type]
    ) as client:
        with console.status("Retrieving latest pushed stacks"):
            r = await client.get(f"git/matching-refs/heads/{stack_prefix}/")
            check_for_status(r)
            refs = typing.cast(list[GitRef], r.json())

            tasks = [
                get_changeid_and_pull(client, user, stack_prefix, ref)
                for ref in refs
                if not ref["ref"].endswith("/aio")
            ]
            if tasks:
                done, _ = await asyncio.wait(tasks)
                for task in done:
                    known_changeids.update(dict([await task]))

        with console.status("Preparing stacked branches..."):
            console.log("Stacked pull request plan:", style="green")
            changes = await get_local_changes(commits, stack_prefix, known_changeids)
            changeids_to_delete = await get_changeids_to_delete(
                changes, known_changeids
            )

        if dry_run:
            console.log("[orange]Finished (dry-run mode) :tada:[/]")
            sys.exit(0)

        console.log("New stacked pull request:", style="green")
        stacked_base_branch = base_branch
        ready_for_review = True
        pulls: list[PullRequest] = []
        continue_create_or_update = True
        for changeid, commit, title, message in changes:
            depends_on = ""
            if pulls:
                depends_on = f"\n\nDepends-On: #{pulls[-1]['number']}"
            stacked_dest_branch = f"{stack_prefix}/{changeid}"
            if continue_create_or_update:
                pull, action = await create_or_update_stack(
                    client,
                    stacked_base_branch,
                    stacked_dest_branch,
                    changeid,
                    commit,
                    title,
                    message + depends_on,
                    ready_for_review,
                    known_changeids,
                )
                pulls.append(pull)
            else:
                action = "skipped"
                pull = known_changeids.get(changeid) or PullRequest(
                    {
                        "title": "<not yet created>",
                        "html_url": "<no-yet-created>",
                        "number": "-1",
                        "node_id": "na",
                        "draft": True,
                        "state": "",
                        "head": {"sha": ""},
                    }
                )

            console.log(
                f"* [blue]\\[{action}][/] '[red]{commit[-7:]}[/] - [b]{pull['title']}[/] {pull['html_url']} - {changeid}"
            )
            stacked_base_branch = stacked_dest_branch
            ready_for_review = False
            if continue_create_or_update and next_only:
                continue_create_or_update = False

        if stack:
            with console.status("Updating comments..."):
                await create_or_update_comments(client, pulls)
            console.log("[green]Comments updated")

        with console.status("Deleting unused branches..."):
            delete_tasks = [
                delete_stack(client, stack_prefix, changeid, known_changeids)
                for changeid in changeids_to_delete
            ]
            if delete_tasks:
                await asyncio.wait(delete_tasks)

        console.log("[green]Finished :tada:[/]")


def GitHubToken(v: str) -> str:
    if not v:
        raise ValueError
    return v


def get_default_branch_prefix() -> str:
    try:
        result = subprocess.check_output(
            "git config --get mrgfy.branch-prefix", shell=True
        )
    except subprocess.CalledProcessError:
        result = b""

    return result.decode().strip() or "mergify_cli"


def cli() -> None:
    global DEBUG
    parser = argparse.ArgumentParser(description="mrgfy")
    parser.add_argument("--debug", action="store_true")
    parser.add_argument("--setup", action="store_true")
    parser.add_argument("--stack", "-s", action="store_true")
    parser.add_argument("--dry-run", "-n", action="store_true")
    parser.add_argument("--next-only", "-x", action="store_true")
    parser.add_argument(
        "--branch-prefix",
        default=get_default_branch_prefix(),
        help="branch prefix used to create stacked PR",
    )
    parser.add_argument(
        "--token",
        default=os.environ.get("GITHUB_TOKEN", ""),
        type=GitHubToken,
        help="GitHub personal access token",
    )
    args = parser.parse_args()
    if args.debug:
        DEBUG = True
    if args.setup:
        asyncio.run(do_setup())
    else:
        asyncio.run(
            main(
                args.token, args.stack, args.next_only, args.branch_prefix, args.dry_run
            )
        )
