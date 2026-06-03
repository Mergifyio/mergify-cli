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

import dataclasses
import json
import subprocess
import typing
from unittest import mock


if typing.TYPE_CHECKING:
    from collections import abc


def assert_stdout_is_single_json_document(stdout: str) -> typing.Any:
    """Assert that ``stdout`` contains exactly one JSON document and return it.

    Formalizes the ``--json`` output discipline: under ``--json`` mode,
    stdout MUST be exactly one JSON document and nothing else — no
    progress bars, no status messages, no prefix or suffix text. Any
    non-JSON content breaks downstream scripts that pipe the output
    into ``jq``, ``python -m json.tool``, or similar.

    ``json.loads`` rejects trailing non-whitespace content, so calling
    this is equivalent to calling ``json.loads(stdout)`` — the helper
    exists to name the invariant and produce a clearer failure message.

    Use this in any test that exercises a ``--json`` code path.
    """
    try:
        return json.loads(stdout)
    except json.JSONDecodeError as e:
        msg = (
            "stdout is not a single JSON document — "
            "likely a progress message, banner, or other text leaked "
            "into the --json output path.\n"
            f"json error: {e}\n"
            f"stdout: {stdout!r}"
        )
        raise AssertionError(msg) from e


class _CommitRequired(typing.TypedDict):
    sha: str
    title: str
    message: str
    change_id: str


class Commit(_CommitRequired, total=False):
    head_ref: str  # existing PR branch ref; overrides slug-based refspec in finalize()
    note: str  # amend-reason note from `refs/notes/mergify/stack`; empty by default


@dataclasses.dataclass
class GitMock:
    _mocked: dict[tuple[str, ...], str] = dataclasses.field(
        init=False,
        default_factory=dict,
    )
    _mocked_errors: list[tuple[str, ...]] = dataclasses.field(
        init=False,
        default_factory=list,
    )
    _commits: list[Commit] = dataclasses.field(init=False, default_factory=list)
    _called: list[tuple[str, ...]] = dataclasses.field(init=False, default_factory=list)

    def mock(self, *args: str, output: str) -> None:
        self._mocked[args] = output

    def mock_error(self, *args: str) -> None:
        """Register a one-shot call that should raise CommandError.

        Each registration is consumed once; registering the same args twice
        means the first two calls raise and subsequent calls fall through to
        ``_mocked`` (or "not mocked").
        """
        self._mocked_errors.append(args)

    def has_been_called_with(self, *args: str) -> bool:
        return args in self._called

    async def __call__(self, *args: str) -> str:
        from mergify_cli import utils

        if args in self._mocked_errors:
            self._mocked_errors.remove(args)
            self._called.append(args)
            raise utils.CommandError(args, 128, b"")

        if args in self._mocked:
            self._called.append(args)
            return self._mocked[args]

        msg = f"git_mock called with `{args}`, not mocked!"
        raise AssertionError(msg)

    def default_cli_args(self) -> None:
        self.mock("config", "--get", "mergify-cli.github-server", output="")
        self.mock("config", "--get", "mergify-cli.stack-keep-pr-title-body", output="")
        self.mock("config", "--get", "mergify-cli.stack-create-as-draft", output="")
        self.mock("config", "--get", "branch.current-branch.merge", output="")
        self.mock("config", "--get", "branch.current-branch.remote", output="")
        self.mock("config", "--get", "mergify-cli.stack-branch-prefix", output="")
        self.mock("merge-base", "--fork-point", "origin/main", output="")

    def commit(self, commit: Commit) -> None:
        self._commits.append(commit)

        # Base commit SHA
        self.mock("merge-base", "--fork-point", "origin/main", output="base_commit_sha")

    def build_local_commits(self) -> list[dict[str, str]]:
        """Return the JSON-shape the Rust `_internal stack-local-commits`
        bridge produces for the registered commits.

        Mirrors the per-commit shape emitted by
        `crates/mergify-stack/src/local_commits.rs::LocalCommit`:
        `{commit_sha, title, message, change_id, slug, note}`.
        Called by the `git_mock` fixture's bridge stub instead of
        running the real subprocess against a real git repo. The
        `slug` is computed via the test-only oracle in
        `mergify_cli/tests/_slug_oracle.py`, which mirrors the
        Rust `mergify-stack::slug::slugify_title` algorithm; the
        Rust crate's unit tests pin the parity. The `note`
        defaults to empty string and individual tests can override
        it via `Commit["note"]`.
        """
        from mergify_cli.tests._slug_oracle import slugify_title

        out: list[dict[str, str]] = []
        for c in self._commits:
            body = f"{c['message']}\n\nChange-Id: {c['change_id']}"
            out.append(
                {
                    "commit_sha": c["sha"],
                    "title": c["title"],
                    "message": body,
                    "change_id": c["change_id"],
                    "slug": slugify_title(c["title"], c["change_id"]),
                    "note": c.get("note", ""),
                },
            )
        return out

    def finalize(
        self,
        *,
        remote_shas: dict[str, str] | None = None,
        no_verify: bool = False,
    ) -> None:
        # Register the rev-parse --verify probe used by fetch_notes_ref
        # (local ref absent → CommandError so the fetch is attempted)
        self.mock_error("rev-parse", "--verify", "refs/notes/mergify/stack")

        # Register the refs/notes/mergify fetch probe (no + since local ref absent)
        self.mock(
            "fetch",
            "origin",
            "--no-write-fetch-head",
            "refs/notes/mergify/stack:refs/notes/mergify/stack",
            output="",
        )

        # The inline `git log --reverse --format=%H…` invocation
        # is gone — `_read_local_commits_via_rust` now shells out
        # to the Rust `_internal stack-local-commits` subcommand,
        # mocked at the helper level (see the `git_mock` fixture
        # in tests/conftest.py). `build_local_commits()` returns
        # the parsed-JSON shape the helper would have produced.

        # Register batch note-read mock (empty = "no note")
        for c in self._commits:
            self.mock(
                "notes",
                "--ref=refs/notes/mergify/stack",
                "show",
                c["sha"],
                output="",
            )

        # Register notes rev-parse --verify (used to check if local ref exists)
        self.mock(
            "rev-parse",
            "--verify",
            "refs/notes/mergify/stack",
            output="fake_notes_sha",
        )

        # Register batch push mock with explicit per-ref leases
        if not self._commits:
            return

        from mergify_cli.tests._slug_oracle import slugify_title

        lease_args: list[str] = []
        refspecs: list[str] = []
        for c in self._commits:
            if "head_ref" in c:
                branch = c["head_ref"]
            else:
                branch = f"current-branch/{slugify_title(c['title'], c['change_id'])}"
            expected_sha = (remote_shas or {}).get(c["change_id"], "")
            lease_args.append(
                f"--force-with-lease=refs/heads/{branch}:{expected_sha}",
            )
            refspecs.append(f"{c['sha']}:refs/heads/{branch}")

        no_verify_args: tuple[str, ...] = ("--no-verify",) if no_verify else ()

        # notes_ref_fetched=True: lease uses the SHA from rev-parse --verify
        notes_lease = "--force-with-lease=refs/notes/mergify/stack:fake_notes_sha"
        self.mock(
            "push",
            "--atomic",
            *no_verify_args,
            *lease_args,
            notes_lease,
            "origin",
            *refspecs,
            "+refs/notes/mergify/stack:refs/notes/mergify/stack",
            output="",
        )


@dataclasses.dataclass
class SubprocessMock:
    cmd: list[str]
    output: str = ""
    exit_code: int = 0


@dataclasses.dataclass
class SubprocessMocks:
    calls: list[SubprocessMock] = dataclasses.field(default_factory=list)

    def register(self, cmd: list[str], output: str = "", exit_code: int = 0) -> None:
        self.calls.append(SubprocessMock(cmd=cmd, output=output, exit_code=exit_code))


def subprocess_mocked() -> abc.Generator[SubprocessMocks]:
    mocks = SubprocessMocks()

    with mock.patch("subprocess.check_output") as mock_run:

        def check_output(
            cmd: list[str],
            **kwargs: typing.Any,  # noqa: ARG001
        ) -> str:
            try:
                m = mocks.calls.pop(0)
            except IndexError:
                msg = f"Unexpected command: {cmd}"
                raise ValueError(msg)
            if m.cmd == cmd:
                if m.exit_code != 0:
                    raise subprocess.CalledProcessError(
                        m.exit_code,
                        cmd,
                        output=m.output,
                    )
                return m.output
            msg = f"Unexpected command: {m.cmd} != {cmd}"
            raise ValueError(msg)

        mock_run.side_effect = check_output
        yield mocks
