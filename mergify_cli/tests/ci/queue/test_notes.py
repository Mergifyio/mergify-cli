from __future__ import annotations

import subprocess
from unittest import mock

from mergify_cli.ci.queue import notes


BRANCH = "mergify/merge-queue/abcdef0123"
HEAD_SHA = "a" * 40
BASE_SHA = "b" * 40
NOTES_REF = f"refs/notes/{BRANCH}"


def _completed(returncode: int = 0) -> subprocess.CompletedProcess[str]:
    return subprocess.CompletedProcess(args=[], returncode=returncode)


def test_read_returns_metadata_when_note_is_valid_yaml() -> None:
    note_yaml = f"""\
scopes:
  - backend
pull_requests:
  - number: 1
    scopes:
      - backend
previous_failed_batches: []
checking_base_sha: {BASE_SHA}
"""

    with (
        mock.patch("subprocess.run", return_value=_completed()) as run_mock,
        mock.patch(
            "subprocess.check_output",
            return_value=note_yaml,
        ) as check_output_mock,
    ):
        result = notes.read_mq_info_note(BRANCH, HEAD_SHA)

    run_mock.assert_called_once()
    fetch_cmd = run_mock.call_args.args[0]
    assert fetch_cmd == [
        "git",
        "fetch",
        "--no-tags",
        "--quiet",
        "origin",
        f"+{NOTES_REF}:{NOTES_REF}",
    ]

    check_output_mock.assert_called_once()
    show_cmd = check_output_mock.call_args.args[0]
    assert show_cmd == ["git", "notes", f"--ref={BRANCH}", "show", HEAD_SHA]

    assert result is not None
    assert result["checking_base_sha"] == BASE_SHA
    assert result["pull_requests"] == [{"number": 1, "scopes": ["backend"]}]
    assert result["previous_failed_batches"] == []


def test_read_returns_none_when_fetch_fails() -> None:
    with (
        mock.patch(
            "subprocess.run",
            side_effect=subprocess.CalledProcessError(128, ["git", "fetch"]),
        ),
        mock.patch("subprocess.check_output") as check_output_mock,
    ):
        result = notes.read_mq_info_note(BRANCH, HEAD_SHA)

    assert result is None
    check_output_mock.assert_not_called()


def test_read_returns_none_when_git_binary_missing() -> None:
    with mock.patch("subprocess.run", side_effect=FileNotFoundError("no git")):
        result = notes.read_mq_info_note(BRANCH, HEAD_SHA)

    assert result is None


def test_read_returns_none_when_note_show_fails() -> None:
    with (
        mock.patch("subprocess.run", return_value=_completed()),
        mock.patch(
            "subprocess.check_output",
            side_effect=subprocess.CalledProcessError(1, ["git", "notes"]),
        ),
    ):
        result = notes.read_mq_info_note(BRANCH, HEAD_SHA)

    assert result is None


def test_read_returns_none_when_yaml_is_invalid() -> None:
    with (
        mock.patch("subprocess.run", return_value=_completed()),
        mock.patch("subprocess.check_output", return_value=": not valid yaml:\n  [["),
    ):
        result = notes.read_mq_info_note(BRANCH, HEAD_SHA)

    assert result is None


def test_read_returns_none_when_yaml_lacks_checking_base_sha() -> None:
    with (
        mock.patch("subprocess.run", return_value=_completed()),
        mock.patch("subprocess.check_output", return_value="pull_requests: []\n"),
    ):
        result = notes.read_mq_info_note(BRANCH, HEAD_SHA)

    assert result is None


def test_read_returns_none_when_checking_base_sha_is_not_a_string() -> None:
    with (
        mock.patch("subprocess.run", return_value=_completed()),
        mock.patch(
            "subprocess.check_output",
            return_value="checking_base_sha: 12345\npull_requests: []\n",
        ),
    ):
        result = notes.read_mq_info_note(BRANCH, HEAD_SHA)

    assert result is None


def test_read_returns_none_when_yaml_is_scalar() -> None:
    with (
        mock.patch("subprocess.run", return_value=_completed()),
        mock.patch("subprocess.check_output", return_value="just-a-string\n"),
    ):
        result = notes.read_mq_info_note(BRANCH, HEAD_SHA)

    assert result is None
