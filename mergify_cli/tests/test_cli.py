import os
import pathlib
import subprocess
import sys
import textwrap


def test_reexec_enables_utf8_and_prints_emoji(
    tmp_path: pathlib.Path,
) -> None:
    """
    Run in a child process so os.execv can safely replace it.
    We patch mymodule.cli inside the child (no mocks), then call main().
    We assert:
      - sys.flags.utf8_mode == 1 at print time
      - the emoji is present in stdout
    """
    script = tmp_path / "runner.py"
    script.write_text(
        textwrap.dedent(
            """
            import sys
            import os

            from mergify_cli import cli

            # Make cli() print a probe + the emoji
            def _cli():
                print(f"utf8_mode={int(sys.flags.utf8_mode)}")
                print("✅")

            cli.cli = _cli
            cli.main()
            """,
        ),
        encoding="utf-8",
    )

    # Force utf8_mode OFF initially so enforce_utf8_mode triggers on Windows.
    env = os.environ.copy()
    env["PYTHONUTF8"] = "0"  # ensure not already in UTF-8 mode

    proc = subprocess.run(
        [sys.executable, str(script)],
        check=False,
        env=env,
        capture_output=True,
        text=True,
    )

    assert proc.returncode == 0, proc.stderr
    stdout = proc.stdout
    if os.name == "nt":
        # After re-exec with -X utf8, utf8_mode should be 1 at print time
        assert "utf8_mode=1" in stdout
    else:
        # No reexec on linux, no need the utf8 mode
        assert "utf8_mode=0" in stdout

    assert "✅" in stdout
