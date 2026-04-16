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

import enum


class ExitCode(enum.IntEnum):
    """Structured exit codes for mergify CLI.

    These exit codes allow scripts and automation to distinguish between
    different failure modes without parsing stderr.

    Code 2 is reserved for Click's built-in usage errors (BadParameter,
    UsageError).
    """

    SUCCESS = 0
    GENERIC_ERROR = 1
    # 2 is reserved for Click usage/parameter errors
    STACK_NOT_FOUND = 3
    CONFLICT = 4
    GITHUB_API_ERROR = 5
    MERGIFY_API_ERROR = 6
    INVALID_STATE = 7
    CONFIGURATION_ERROR = 8
