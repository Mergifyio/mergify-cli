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

import subprocess
from unittest import mock

import pytest

import mergify_cli


def test_cli_help(capsys: pytest.CaptureFixture[str]) -> None:
    with pytest.raises(SystemExit, match="0"):
        with mock.patch(
            "subprocess.check_output",
            side_effect=subprocess.CalledProcessError(2, ""),
        ):
            mergify_cli.parse_args(["--help"])

    stdout = capsys.readouterr().out
    assert "usage: " in stdout
    assert "positional arguments:" in stdout
    assert "options:" in stdout
