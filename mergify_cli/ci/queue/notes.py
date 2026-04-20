"""Read a Mergify merge-queue info note from the current git repository.

The engine publishes MQ batch metadata as a git note on the draft branch's head
commit under `refs/notes/<mq_branch_name>`. Reading it requires only a git
fetch, no GitHub token — handy for CI providers like Buildkite that don't ship
the webhook payload to the build.

All errors (missing ref, missing note, bad YAML) are swallowed and reported as
`None` so callers can fall back to legacy detection paths.
"""

from __future__ import annotations

import subprocess
import typing

import yaml


if typing.TYPE_CHECKING:
    from mergify_cli.ci.queue import metadata


def read_mq_info_note(
    branch_name: str,
    head_sha: str,
) -> metadata.MergeQueueMetadata | None:
    notes_ref = f"refs/notes/{branch_name}"

    # The engine force-updates the notes ref on MQ retries (fresh commit, no
    # parents), so the remote SHA moves non-linearly. A '+' on the refspec is
    # required to accept the non-fast-forward update locally.
    try:
        subprocess.run(  # noqa: S603
            [
                "git",
                "fetch",
                "--no-tags",
                "--quiet",
                "origin",
                f"+{notes_ref}:{notes_ref}",
            ],
            check=True,
            capture_output=True,
        )
    except (OSError, subprocess.CalledProcessError):
        return None

    try:
        content = subprocess.check_output(  # noqa: S603
            ["git", "notes", f"--ref={branch_name}", "show", head_sha],
            text=True,
            encoding="utf-8",
            stderr=subprocess.DEVNULL,
        )
    except (OSError, subprocess.CalledProcessError):
        return None

    try:
        data = yaml.safe_load(content)
    except yaml.YAMLError:
        return None

    if not isinstance(data, dict) or not isinstance(
        data.get("checking_base_sha"),
        str,
    ):
        return None

    return typing.cast("metadata.MergeQueueMetadata", data)
