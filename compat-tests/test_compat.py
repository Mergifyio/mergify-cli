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
"""Cross-implementation compat-test runner.

Discovers fixtures under ``compat-tests/cases/<case-name>/`` and runs
them against ``python -m mergify_cli``. Asserts the observed exit
code matches ``expected_exit`` and the optional ``stdout_contains``
substring appears in stdout.

When the Rust implementation exists, this runner will be extended to
also invoke the Rust binary against each case and diff the results.
See ``compat-tests/README.md``.
"""

from __future__ import annotations

import pathlib
import subprocess
import sys

import pytest


CASES_DIR = pathlib.Path(__file__).parent / "cases"


def _discover_cases() -> list[pathlib.Path]:
    if not CASES_DIR.is_dir():
        return []
    return sorted(p for p in CASES_DIR.iterdir() if p.is_dir())


@pytest.mark.parametrize(
    "case_dir",
    _discover_cases(),
    ids=lambda p: p.name,
)
def test_compat(case_dir: pathlib.Path) -> None:
    args = (case_dir / "args").read_text().strip().split()
    expected_exit = int((case_dir / "expected_exit").read_text().strip())

    # Close stdin so any accidental interactive prompt (questionary,
    # `input()`, etc.) fails fast instead of blocking the test run.
    # `timeout=30` caps pathological hangs at 30s — compat cases are
    # expected to be short; anything longer is a bug.
    result = subprocess.run(
        [sys.executable, "-m", "mergify_cli", *args],
        capture_output=True,
        text=True,
        check=False,
        stdin=subprocess.DEVNULL,
        timeout=30,
    )

    assert result.returncode == expected_exit, (
        f"expected exit {expected_exit}, got {result.returncode}\n"
        f"stdout:\n{result.stdout}\n"
        f"stderr:\n{result.stderr}"
    )

    stdout_contains_file = case_dir / "stdout_contains"
    if stdout_contains_file.exists():
        expected_substr = stdout_contains_file.read_text().strip()
        assert expected_substr in result.stdout, (
            f"expected stdout to contain {expected_substr!r}\n"
            f"actual stdout:\n{result.stdout}"
        )
