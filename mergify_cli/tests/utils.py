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
from collections import abc
import dataclasses
import subprocess
import typing
from unittest import mock


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
        self.mock("config", "--get", "mergify-cli.stack-create-as-draft", output="")
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


@dataclasses.dataclass
class SubprocessMock:
    cmd: list[str]
    output: str = ""
    exit_code: int = 0


@dataclasses.dataclass
class SubprocessMocks:
    calls: list[SubprocessMock] = dataclasses.field(default_factory=list)

    def register(self, cmd: list[str], output: str = "", exit_code: int = 0) -> None:
        self.calls.append(SubprocessMock(cmd=cmd, output=output, exit_code=exit_code))


def subprocess_mocked() -> abc.Generator[SubprocessMocks]:
    mocks = SubprocessMocks()

    with mock.patch("subprocess.check_output") as mock_run:

        def check_output(
            cmd: list[str],
            **kwargs: typing.Any,  # noqa: ARG001 ANN401
        ) -> str:
            try:
                m = mocks.calls.pop(0)
            except IndexError:
                msg = f"Unexpected command: {cmd}"
                raise ValueError(msg)
            if m.cmd == cmd:
                if m.exit_code != 0:
                    raise subprocess.CalledProcessError(
                        m.exit_code,
                        cmd,
                        output=m.output,
                    )
                return m.output
            msg = f"Unexpected command: {m.cmd} != {cmd}"
            raise ValueError(msg)

        mock_run.side_effect = check_output
        yield mocks
