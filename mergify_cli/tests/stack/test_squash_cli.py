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

import click
import pytest

from mergify_cli.stack.cli import _parse_squash_tokens


class TestParseSquashTokens:
    def test_single_src(self) -> None:
        srcs, target = _parse_squash_tokens(("A", "into", "X"))
        assert srcs == ["A"]
        assert target == "X"

    def test_multiple_srcs(self) -> None:
        srcs, target = _parse_squash_tokens(("A", "B", "C", "into", "X"))
        assert srcs == ["A", "B", "C"]
        assert target == "X"

    def test_missing_into_errors(self) -> None:
        with pytest.raises(click.BadParameter):
            _parse_squash_tokens(("A", "B", "C", "X"))

    def test_two_intos_errors(self) -> None:
        with pytest.raises(click.BadParameter):
            _parse_squash_tokens(("A", "into", "B", "into", "X"))

    def test_no_srcs_errors(self) -> None:
        with pytest.raises(click.BadParameter):
            _parse_squash_tokens(("into", "X"))

    def test_no_target_errors(self) -> None:
        with pytest.raises(click.BadParameter):
            _parse_squash_tokens(("A", "into"))

    def test_multiple_targets_errors(self) -> None:
        with pytest.raises(click.BadParameter):
            _parse_squash_tokens(("A", "into", "X", "Y"))
