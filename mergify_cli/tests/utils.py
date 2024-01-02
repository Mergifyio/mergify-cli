#
#  Copyright © 2021-2023 Mergify SAS
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


@dataclasses.dataclass
class GitMock:
    _mocked: dict[str, str] = dataclasses.field(init=False, default_factory=dict)

    def mock(self, command: str, output: str) -> None:
        self._mocked[command] = output

    async def __call__(self, args: str) -> str:
        if args in self._mocked:
            return self._mocked[args]

        raise AssertionError(f"git_mock called with `{args}`, not mocked!")
