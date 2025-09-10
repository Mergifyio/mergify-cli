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

import os
import subprocess
import sys

import click
import click.decorators
import click_default_group

from mergify_cli import VERSION
from mergify_cli.ci import cli as ci_cli_mod
from mergify_cli.stack import cli as stack_cli_mod


@click.group(
    cls=click_default_group.DefaultGroup,
    default="stack",
    default_if_no_args=True,
)
@click.option("--debug", is_flag=True, default=False, help="debug mode")
@click.version_option(VERSION)
@click.pass_context
def cli(
    ctx: click.Context,
    debug: bool,
) -> None:
    ctx.obj = {"debug": debug}


cli.add_command(stack_cli_mod.stack)
cli.add_command(ci_cli_mod.ci)


def enforce_utf8_mode() -> None:
    if sys.flags.utf8_mode:
        return

    argv = [sys.executable, "-X", "utf8"]
    argv.extend(subprocess._args_from_interpreter_flags())  # type: ignore[attr-defined]  # noqa: SLF001
    argv.extend(sys.argv)

    os.execv(argv[0], argv)  # noqa: S606


def main() -> None:
    # NOTE:
    #   It's unlikely this day that a platform does not support unicode.
    #   But on windows, the default encoding may not be an unicode one.
    #   Let's try our best by forcing utf-8 and if it's impossible, just returns escaped character
    if os.name == "nt":
        enforce_utf8_mode()
    cli()
