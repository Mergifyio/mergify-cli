from __future__ import annotations

import os
import subprocess


def test_reexec_enables_utf8_and_prints_emoji() -> None:
    """
    Run in a child process so os.execv can safely replace it.
    We patch mymodule.cli inside the child (no mocks), then call main().
    We assert:
      - sys.flags.utf8_mode == 1 at print time
      - the emoji is present in stdout
    """
    # Force utf8_mode OFF initially so enforce_utf8_mode triggers on Windows.
    env = os.environ.copy()
    env["PYTHONUTF8"] = "0"  # ensure not already in UTF-8 mode
    env["MERGIFY_CLI_TESTING_UTF8_MODE"] = "1"

    # We need to use shell to get binary PATH lookup working on windows
    proc = subprocess.run(  # noqa: S602
        "mergify --help",
        check=False,
        env=env,
        capture_output=True,
        text=True,
        shell=True,
    )

    assert proc.returncode == 0, proc.stderr  # noqa: S101
    stdout = proc.stdout
    if os.name == "nt":
        # After re-exec with -X utf8, utf8_mode should be 1 at print time
        assert "utf8_mode=1" in stdout  # noqa: S101
    else:
        # No reexec on linux, no need the utf8 mode
        assert "utf8_mode=0" in stdout  # noqa: S101

    assert "✅" in stdout  # noqa: S101
