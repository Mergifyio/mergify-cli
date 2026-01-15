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

"""Claude session ID management for mergify-cli stack."""

from __future__ import annotations

import shutil
import subprocess

from mergify_cli import console
from mergify_cli import utils
from mergify_cli.stack.changes import CLAUDE_SESSION_ID_RE
from mergify_cli.stack.changes import ClaudeSessionId


async def get_session_id_from_commit(
    commit_sha: str = "HEAD",
) -> ClaudeSessionId | None:
    """Extract Claude-Session-Id from a commit message."""
    message = await utils.git("log", "-1", "--format=%B", commit_sha)
    match = CLAUDE_SESSION_ID_RE.search(message)
    if match:
        return ClaudeSessionId(match.group(1))
    return None


def launch_claude_session(session_id: str) -> None:
    """Launch Claude Code with the given session ID."""
    claude_path = shutil.which("claude")
    if not claude_path:
        console.print("Error: 'claude' command not found in PATH", style="red")
        return

    console.print(f"Launching Claude with session: {session_id}")
    result = subprocess.run([claude_path, "--resume", session_id], check=False)  # noqa: S603
    if result.returncode != 0:
        console.print(
            f"Error: 'claude' exited with code {result.returncode}",
            style="red",
        )
