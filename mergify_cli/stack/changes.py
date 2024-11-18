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


import asyncio
import dataclasses
import re
import sys
import typing

import httpx

from mergify_cli import console
from mergify_cli import github_types
from mergify_cli import utils


CHANGEID_RE = re.compile(r"Change-Id: (I[0-9a-z]{40})")

ChangeId = typing.NewType("ChangeId", str)
RemoteChanges = typing.NewType(
    "RemoteChanges",
    dict[ChangeId, github_types.PullRequest],
)

ActionT = typing.Literal[
    "skip-merged",
    "skip-next-only",
    "skip-create",
    "skip-up-to-date",
    "create",
    "update",
]


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


async def get_remote_changes(
    client: httpx.AsyncClient,
    user: str,
    repo: str,
    stack_prefix: str,
    author: str,
) -> RemoteChanges:
    r = await client.get(
        "/search/issues",
        params={
            "q": f"repo:{user}/{repo} author:{author} is:pull-request head:{stack_prefix}",
            "per_page": 100,
            "sort": "updated",
        },
    )

    responses = await asyncio.gather(
        *(client.get(item["pull_request"]["url"]) for item in r.json()["items"]),
    )
    pulls = [typing.cast(github_types.PullRequest, r.json()) for r in responses]

    remote_changes = RemoteChanges({})
    for pull in pulls:
        # Drop closed but not merged PR
        if pull["state"] == "closed" and pull["merged_at"] is None:
            continue

        changeid = ChangeId(pull["head"]["ref"].split("/")[-1])

        if changeid in remote_changes:
            other_pull = remote_changes[changeid]
            if other_pull["state"] == "closed" and pull["state"] == "closed":
                # Keep the more recent
                pass
            elif other_pull["state"] == "closed" and pull["state"] == "open":
                remote_changes[changeid] = pull
            elif other_pull["state"] == "opened":
                msg = f"More than 1 pull found with this head: {pull['head']['ref']}"
                raise RuntimeError(msg)

        else:
            remote_changes[changeid] = pull

    return remote_changes


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

    def get_log_from_local_change(
        self,
        dry_run: bool,
        create_as_draft: bool,
    ) -> str:
        url = f"<{self.dest_branch}>" if self.pull is None else self.pull["html_url"]

        flags: str = ""
        if self.pull and self.pull["draft"]:
            flags += " [yellow](draft)[/]"

        if self.action == "create":
            color = "yellow" if dry_run else "blue"
            action = "to create" if dry_run else "created"
            commit_info = self.commit_short_sha
            if create_as_draft:
                flags += " [yellow](draft)[/]"

        elif self.action == "update":
            color = "yellow" if dry_run else "blue"
            action = "to update" if dry_run else "updated"
            commit_info = f"{self.pull_short_head_sha} -> {self.commit_short_sha}"

        elif self.action == "skip-create":
            color = "grey"
            action = "skip, --only-update-existing-pulls"
            commit_info = self.commit_short_sha

        elif self.action == "skip-merged":
            color = "purple"
            action = "merged"
            flags += " [purple](merged)[/]"
            commit_info = (
                f"{self.pull['merge_commit_sha'][7:]}"
                if self.pull
                and self.pull["merged_at"]
                and self.pull["merge_commit_sha"]
                else self.commit_short_sha
            )

        elif self.action == "skip-next-only":
            color = "grey"
            action = "skip, --next-only"
            commit_info = self.commit_short_sha

        elif self.action == "skip-up-to-date":
            color = "grey"
            action = "up-to-date"
            commit_info = self.commit_short_sha

        else:
            # NOTE: we don't want to miss any action
            msg = f"Unhandled action: {self.action}"  # type: ignore[unreachable]
            raise RuntimeError(msg)

        return f"* [{color}]\\[{action}][/] '[red]{commit_info}[/] - [b]{self.title}[/]{flags} {url}"


@dataclasses.dataclass
class OrphanChange(Change):
    def get_log_from_orphan_change(self, dry_run: bool) -> str:
        action = "to delete" if dry_run else "deleted"
        title = self.pull["title"] if self.pull else "<unknown>"
        url = self.pull["html_url"] if self.pull else "<unknown>"
        sha = self.pull["head"]["sha"][7:] if self.pull else "<unknown>"
        return f"* [red]\\[{action}][/] '[red]{sha}[/] - [b]{title}[/] {url}"


@dataclasses.dataclass
class Changes:
    stack_prefix: str
    locals: list[LocalChange] = dataclasses.field(default_factory=list)
    orphans: list[OrphanChange] = dataclasses.field(default_factory=list)


def display_plan(
    changes: Changes,
    create_as_draft: bool,
) -> None:
    for change in changes.locals:
        console.log(
            change.get_log_from_local_change(
                dry_run=True,
                create_as_draft=create_as_draft,
            ),
        )

    for orphan in changes.orphans:
        console.log(orphan.get_log_from_orphan_change(dry_run=True))


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
                await utils.git(
                    "log",
                    "--format=%H",
                    f"{base_commit_sha}..{dest_branch}",
                )
            ).split(
                "\n",
            ),
        )
        if commit
    )
    changes = Changes(stack_prefix)
    remaining_remote_changes = remote_changes.copy()

    for idx, commit in enumerate(commits):
        message = await utils.git("log", "-1", "--format=%b", commit)
        title = await utils.git("log", "-1", "--format=%s", commit)

        changeids = CHANGEID_RE.findall(message)
        if not changeids:
            console.print(
                f"`Change-Id:` line is missing on commit {commit}",
                style="red",
            )
            console.print(
                "Did you run `mergify stack --setup` for this repository?",
            )
            # TODO(sileht): we should raise an Exception and exit in main program
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
        if pull["state"] == "open":
            changes.orphans.append(OrphanChange(changeid, pull))

    return changes
