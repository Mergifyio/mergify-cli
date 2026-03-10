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

import json
from unittest import mock

import pytest

from mergify_cli.stack import dashboard as dashboard_mod


def _make_gh_pr(
    number: int,
    title: str,
    repo: str = "Mergifyio/engine",
    *,
    is_draft: bool = False,
    body: str = "",
    labels: list[str] | None = None,
) -> dict[str, object]:
    return {
        "number": number,
        "title": title,
        "url": f"https://github.com/{repo}/pull/{number}",
        "repository": {"nameWithOwner": repo},
        "isDraft": is_draft,
        "body": body,
        "labels": [{"name": label} for label in (labels or [])],
    }


def _make_ci_response(*, success: bool) -> str:
    if success:
        return json.dumps(
            {
                "statusCheckRollup": [
                    {"conclusion": "SUCCESS", "status": "COMPLETED"},
                ],
            },
        )
    return json.dumps(
        {
            "statusCheckRollup": [
                {"conclusion": "FAILURE", "status": "COMPLETED"},
            ],
        },
    )


class _RunCommandMock:
    """Mock for utils.run_command that dispatches based on command args."""

    def __init__(self) -> None:
        self.responses: dict[tuple[str, ...], str] = {}

    def register(self, *args: str, output: str) -> None:
        self.responses[args] = output

    def register_gh_search(self, query: str, results: list[dict[str, object]]) -> None:
        self.responses[
            "gh",
            "search",
            "prs",
            "--json",
            "number,title,url,repository,isDraft,body,labels",
            "--limit",
            "200",
            query,
        ] = json.dumps(results)

    def register_gh_ci(self, repo: str, pr_number: int, *, success: bool) -> None:
        self.responses[
            "gh",
            "pr",
            "view",
            str(pr_number),
            "--repo",
            repo,
            "--json",
            "statusCheckRollup",
        ] = _make_ci_response(success=success)

    async def __call__(self, *args: str) -> str:
        if args in self.responses:
            return self.responses[args]
        msg = f"run_command called with `{args}`, not mocked!"
        raise AssertionError(msg)


@pytest.fixture
def run_command_mock() -> _RunCommandMock:
    cmd_mock = _RunCommandMock()
    cmd_mock.register("gh", "api", "user", "--jq", ".login", output="julian")
    return cmd_mock


async def test_dashboard_standalone_prs(
    run_command_mock: _RunCommandMock,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Test dashboard with standalone PRs (no stacks)."""
    run_command_mock.register_gh_search(
        "is:open is:pr author:julian org:Mergifyio draft:true",
        [_make_gh_pr(100, "WIP feature", is_draft=True)],
    )
    run_command_mock.register_gh_search(
        "is:open is:pr author:julian org:Mergifyio draft:false",
        [_make_gh_pr(101, "Ready feature")],
    )
    run_command_mock.register_gh_search(
        "is:open is:pr review-requested:julian draft:false",
        [_make_gh_pr(200, "Review me")],
    )
    run_command_mock.register_gh_ci("Mergifyio/engine", 200, success=True)

    with mock.patch("mergify_cli.stack.dashboard.utils.run_command", run_command_mock):
        await dashboard_mod.stack_dashboard(
            org="Mergifyio",
            author="julian",
            exclude_labels=[],
        )

    captured = capsys.readouterr()
    assert "Work in Progress" in captured.out
    assert "WIP feature" in captured.out
    assert "Awaiting Team Review" in captured.out
    assert "Ready feature" in captured.out
    assert "Awaiting My Review" in captured.out
    assert "Review me" in captured.out


async def test_dashboard_stack_tree_view(
    run_command_mock: _RunCommandMock,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Test that stacked PRs are displayed as a tree."""
    stack_prs = [
        _make_gh_pr(10, "Base change"),
        _make_gh_pr(11, "Middle change", body="Depends-On: #10"),
        _make_gh_pr(12, "Top change", body="Depends-On: #11"),
    ]

    run_command_mock.register_gh_search(
        "is:open is:pr author:julian org:Mergifyio draft:true",
        [],
    )
    run_command_mock.register_gh_search(
        "is:open is:pr author:julian org:Mergifyio draft:false",
        stack_prs,
    )
    run_command_mock.register_gh_search(
        "is:open is:pr review-requested:julian draft:false",
        [],
    )

    with mock.patch("mergify_cli.stack.dashboard.utils.run_command", run_command_mock):
        await dashboard_mod.stack_dashboard(
            org="Mergifyio",
            author="julian",
            exclude_labels=[],
        )

    captured = capsys.readouterr()
    # Tree connectors should be present
    assert "├──" in captured.out
    assert "└──" in captured.out
    # All PRs in the stack should be shown
    assert "#10" in captured.out
    assert "#11" in captured.out
    assert "#12" in captured.out
    assert "Base change" in captured.out
    assert "Top change" in captured.out


async def test_dashboard_repo_grouping(
    run_command_mock: _RunCommandMock,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Test that PRs are grouped by repo."""
    run_command_mock.register_gh_search(
        "is:open is:pr author:julian org:Mergifyio draft:false",
        [
            _make_gh_pr(10, "Engine PR", repo="Mergifyio/engine"),
            _make_gh_pr(20, "CLI PR", repo="Mergifyio/mergify-cli"),
        ],
    )
    run_command_mock.register_gh_search(
        "is:open is:pr author:julian org:Mergifyio draft:true",
        [],
    )
    run_command_mock.register_gh_search(
        "is:open is:pr review-requested:julian draft:false",
        [],
    )

    with mock.patch("mergify_cli.stack.dashboard.utils.run_command", run_command_mock):
        await dashboard_mod.stack_dashboard(
            org="Mergifyio",
            author="julian",
            exclude_labels=[],
        )

    captured = capsys.readouterr()
    assert "Mergifyio/engine" in captured.out
    assert "Mergifyio/mergify-cli" in captured.out


async def test_dashboard_exclude_labels(
    run_command_mock: _RunCommandMock,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Test that PRs with excluded labels are hidden."""
    run_command_mock.register_gh_search(
        "is:open is:pr author:julian org:Mergifyio draft:true",
        [],
    )
    run_command_mock.register_gh_search(
        "is:open is:pr author:julian org:Mergifyio draft:false",
        [],
    )
    run_command_mock.register_gh_search(
        "is:open is:pr review-requested:julian draft:false",
        [
            _make_gh_pr(200, "Good PR"),
            _make_gh_pr(201, "Conflicting PR", labels=["conflicts"]),
            _make_gh_pr(202, "Unresolved PR", labels=["review threads unresolved"]),
        ],
    )
    run_command_mock.register_gh_ci("Mergifyio/engine", 200, success=True)

    with mock.patch("mergify_cli.stack.dashboard.utils.run_command", run_command_mock):
        await dashboard_mod.stack_dashboard(
            org="Mergifyio",
            author="julian",
            exclude_labels=["conflicts", "review threads unresolved"],
        )

    captured = capsys.readouterr()
    assert "Good PR" in captured.out
    assert "Conflicting PR" not in captured.out
    assert "Unresolved PR" not in captured.out
    assert "2 PRs hidden" in captured.out


async def test_dashboard_exclude_failing_ci(
    run_command_mock: _RunCommandMock,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Test that PRs with failing CI are hidden."""
    run_command_mock.register_gh_search(
        "is:open is:pr author:julian org:Mergifyio draft:true",
        [],
    )
    run_command_mock.register_gh_search(
        "is:open is:pr author:julian org:Mergifyio draft:false",
        [],
    )
    run_command_mock.register_gh_search(
        "is:open is:pr review-requested:julian draft:false",
        [
            _make_gh_pr(300, "CI green PR"),
            _make_gh_pr(301, "CI red PR"),
        ],
    )
    run_command_mock.register_gh_ci("Mergifyio/engine", 300, success=True)
    run_command_mock.register_gh_ci("Mergifyio/engine", 301, success=False)

    with mock.patch("mergify_cli.stack.dashboard.utils.run_command", run_command_mock):
        await dashboard_mod.stack_dashboard(
            org="Mergifyio",
            author="julian",
            exclude_labels=[],
        )

    captured = capsys.readouterr()
    assert "CI green PR" in captured.out
    assert "CI red PR" not in captured.out
    assert "1 PRs hidden" in captured.out


async def test_dashboard_json_output(
    run_command_mock: _RunCommandMock,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Test JSON output format."""
    run_command_mock.register_gh_search(
        "is:open is:pr author:julian org:Mergifyio draft:true",
        [_make_gh_pr(100, "Draft PR", is_draft=True)],
    )
    run_command_mock.register_gh_search(
        "is:open is:pr author:julian org:Mergifyio draft:false",
        [],
    )
    run_command_mock.register_gh_search(
        "is:open is:pr review-requested:julian draft:false",
        [],
    )

    with mock.patch("mergify_cli.stack.dashboard.utils.run_command", run_command_mock):
        await dashboard_mod.stack_dashboard(
            org="Mergifyio",
            author="julian",
            exclude_labels=[],
            output_json=True,
        )

    captured = capsys.readouterr()
    output = json.loads(captured.out)
    assert output["author"] == "julian"
    assert output["org"] == "Mergifyio"
    assert len(output["sections"]) == 3
    assert output["sections"][0]["title"] == "Work in Progress"
    assert "Mergifyio/engine" in output["sections"][0]["repos"]


async def test_dashboard_empty(
    run_command_mock: _RunCommandMock,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Test dashboard with no PRs at all."""
    run_command_mock.register_gh_search(
        "is:open is:pr author:julian org:Mergifyio draft:true",
        [],
    )
    run_command_mock.register_gh_search(
        "is:open is:pr author:julian org:Mergifyio draft:false",
        [],
    )
    run_command_mock.register_gh_search(
        "is:open is:pr review-requested:julian draft:false",
        [],
    )

    with mock.patch("mergify_cli.stack.dashboard.utils.run_command", run_command_mock):
        await dashboard_mod.stack_dashboard(
            org="Mergifyio",
            author="julian",
            exclude_labels=[],
        )

    captured = capsys.readouterr()
    assert "(none)" in captured.out


async def test_dashboard_multi_repo_stacks(
    run_command_mock: _RunCommandMock,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Test that Depends-On only links PRs within the same repo."""
    run_command_mock.register_gh_search(
        "is:open is:pr author:julian org:Mergifyio draft:true",
        [],
    )
    run_command_mock.register_gh_search(
        "is:open is:pr author:julian org:Mergifyio draft:false",
        [
            # PR #10 in engine
            _make_gh_pr(10, "Engine base", repo="Mergifyio/engine"),
            # PR #10 in cli (same number, different repo - not a stack)
            _make_gh_pr(10, "CLI feature", repo="Mergifyio/mergify-cli"),
            # PR #11 in engine depends on #10 (same repo = stack)
            _make_gh_pr(
                11,
                "Engine top",
                repo="Mergifyio/engine",
                body="Depends-On: #10",
            ),
        ],
    )
    run_command_mock.register_gh_search(
        "is:open is:pr review-requested:julian draft:false",
        [],
    )

    with mock.patch("mergify_cli.stack.dashboard.utils.run_command", run_command_mock):
        await dashboard_mod.stack_dashboard(
            org="Mergifyio",
            author="julian",
            exclude_labels=[],
        )

    captured = capsys.readouterr()
    # Engine should show a stack tree
    assert "├──" in captured.out or "└──" in captured.out
    # CLI feature should be standalone (no tree)
    assert "CLI feature" in captured.out


def test_build_stacks_ordering() -> None:
    """Test that _build_stacks returns PRs ordered bottom-to-top."""
    prs = [
        dashboard_mod.DashboardPR(
            number=3,
            title="Top",
            url="",
            repo="r",
            is_draft=False,
            body="Depends-On: #2",
            labels=[],
        ),
        dashboard_mod.DashboardPR(
            number=1,
            title="Bottom",
            url="",
            repo="r",
            is_draft=False,
            body="",
            labels=[],
        ),
        dashboard_mod.DashboardPR(
            number=2,
            title="Middle",
            url="",
            repo="r",
            is_draft=False,
            body="Depends-On: #1",
            labels=[],
        ),
    ]

    stacks = dashboard_mod._build_stacks(prs)
    assert len(stacks) == 1
    assert [pr.number for pr in stacks[0]] == [1, 2, 3]


def test_build_stacks_multiple_independent() -> None:
    """Test that independent PRs form separate stacks."""
    prs = [
        dashboard_mod.DashboardPR(
            number=1,
            title="PR A",
            url="",
            repo="r",
            is_draft=False,
            body="",
            labels=[],
        ),
        dashboard_mod.DashboardPR(
            number=2,
            title="PR B",
            url="",
            repo="r",
            is_draft=False,
            body="",
            labels=[],
        ),
    ]

    stacks = dashboard_mod._build_stacks(prs)
    assert len(stacks) == 2
