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
    _mocked: dict[str, str] = dataclasses.field(init=False, default_factory=dict)
    _commits: list[Commit] = dataclasses.field(init=False, default_factory=list)
    _called: list[str] = dataclasses.field(init=False, default_factory=list)

    def mock(self, command: str, output: str) -> None:
        self._mocked[command] = output

    def has_been_called_with(self, args: str) -> bool:
        return args in self._called

    async def __call__(self, args: str) -> str:
        if args in self._mocked:
            self._called.append(args)
            return self._mocked[args]

        msg = f"git_mock called with `{args}`, not mocked!"
        raise AssertionError(msg)

    def commit(self, commit: Commit) -> None:
        self._commits.append(commit)

        # Base commit SHA
        self.mock("merge-base --fork-point origin/main", "base_commit_sha")
        # Commit message
        self.mock(
            f"log -1 --format='%b' {commit['sha']}",
            f"{commit['message']}\n\nChange-Id: {commit['change_id']}",
        )
        # Commit title
        self.mock(f"log -1 --format='%s' {commit['sha']}", commit["title"])
        # List of commit SHAs
        self.mock(
            "log --format='%H' base_commit_sha..current-branch",
            "\n".join(c["sha"] for c in reversed(self._commits)),
        )
