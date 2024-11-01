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

import importlib.metadata
import pathlib
import shutil
import sys

import aiofiles

from mergify_cli import console
from mergify_cli import utils


async def stack_setup() -> None:
    hooks_dir = pathlib.Path(await utils.git("rev-parse", "--git-path", "hooks"))
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
