#
#  Copyright © 2021-2026 Mergify SAS
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

import difflib

import click


class DYMGroup(click.Group):
    """A click Group that suggests similar command names on typos."""

    def resolve_command(
        self,
        ctx: click.Context,
        args: list[str],
    ) -> tuple[str, click.Command, list[str]]:
        try:
            return super().resolve_command(ctx, args)
        except click.UsageError as e:
            matches = difflib.get_close_matches(
                args[0],
                self.list_commands(ctx),
                n=3,
                cutoff=0.6,
            )
            if matches:
                suggestion = ", ".join(repr(m) for m in matches)
                e.message += f"\n\nDid you mean one of these?\n    {suggestion}"
            raise
