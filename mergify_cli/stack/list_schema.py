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
"""Schema lock for `mergify stack list --json` output.

Mirrors ``StackListOutput.to_dict()`` exactly. The test suite validates
the command's JSON output against ``StackListJsonOutput`` with
``extra="forbid"`` so any drift — extra fields, missing fields, wrong
types, or new literal values — fails loudly.

This is part of the machine-readable compat contract that the Rust
port must preserve byte-for-byte. Changing any field here is a
breaking change to downstream scripts and requires a coordinated
announcement.

`freeze list`, `queue status`, and `queue show` are not locked here
— those commands pass the Mergify API response through unchanged,
so their schema is the API's contract, not ours.
"""

from __future__ import annotations

import typing

import pydantic


class _StrictSchemaBase(pydantic.BaseModel):
    """Base for schema-lock models.

    ``extra="forbid"`` rejects unknown fields; ``strict=True`` disables
    Pydantic's type coercion so a drift like emitting an int as a string
    (or vice versa) fails validation instead of passing silently.
    """

    model_config = pydantic.ConfigDict(extra="forbid", strict=True)


class CICheckJson(_StrictSchemaBase):
    name: str
    status: str


class ReviewJson(_StrictSchemaBase):
    user: str
    state: str


class StackEntryJson(_StrictSchemaBase):
    commit_sha: str
    title: str
    change_id: str
    status: typing.Literal["merged", "draft", "open", "no_pr"]
    pull_number: int | None
    pull_url: str | None
    ci_status: typing.Literal["passing", "failing", "pending", "unknown"]
    ci_checks: list[CICheckJson]
    review_status: typing.Literal[
        "approved",
        "changes_requested",
        "pending",
        "unknown",
    ]
    reviews: list[ReviewJson]
    mergeable: bool | None


class StackListJsonOutput(_StrictSchemaBase):
    branch: str
    trunk: str
    entries: list[StackEntryJson]
