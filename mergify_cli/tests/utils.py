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
import dataclasses
import typing


class Commit(typing.TypedDict):
    sha: str
    title: str
    message: str
    change_id: str


@dataclasses.dataclass
class GitMock:
    _mocked: dict[tuple[str, ...], str] = dataclasses.field(
        init=False,
        default_factory=dict,
    )
    _commits: list[Commit] = dataclasses.field(init=False, default_factory=list)
    _called: list[tuple[str, ...]] = dataclasses.field(init=False, default_factory=list)

    def mock(self, *args: str, output: str) -> None:
        self._mocked[args] = output

    def has_been_called_with(self, *args: str) -> bool:
        return args in self._called

    async def __call__(self, *args: str) -> str:
        if args in self._mocked:
            self._called.append(args)
            return self._mocked[args]

        msg = f"git_mock called with `{args}`, not mocked!"
        raise AssertionError(msg)

    def default_cli_args(self) -> None:
        self.mock("config", "--get", "mergify-cli.github-server", output="")
        self.mock("config", "--get", "mergify-cli.stack-keep-pr-title-body", output="")
        self.mock("config", "--get", "branch.current-branch.merge", output="")
        self.mock("config", "--get", "branch.current-branch.remote", output="")
        self.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")
        self.mock("merge-base", "--fork-point", "origin/main", output="")

    def commit(self, commit: Commit) -> None:
        self._commits.append(commit)

        # Base commit SHA
        self.mock("merge-base", "--fork-point", "origin/main", output="base_commit_sha")
        # Commit message
        self.mock(
            "log",
            "-1",
            "--format=%b",
            commit["sha"],
            output=f"{commit['message']}\n\nChange-Id: {commit['change_id']}",
        )
        # Commit title
        self.mock("log", "-1", "--format=%s", commit["sha"], output=commit["title"])
        # List of commit SHAs
        self.mock(
            "log",
            "--format=%H",
            "base_commit_sha..current-branch",
            output="\n".join(c["sha"] for c in reversed(self._commits)),
        )
        self.mock("branch", "mergify-cli-tmp", commit["sha"], output="")
        self.mock("branch", "-D", "mergify-cli-tmp", output="")
        self.mock(
            "push",
            "-f",
            "origin",
            f"mergify-cli-tmp:current-branch/{commit['change_id']}",
            output="",
        )
