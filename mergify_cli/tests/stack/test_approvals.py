#
#  Copyright © 2026 Mergify SAS
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

from typing import TYPE_CHECKING
from unittest import mock

import httpx
import pytest

from mergify_cli.stack import approvals
from mergify_cli.stack import changes


if TYPE_CHECKING:
    import respx


def _pull(number: int) -> dict[str, object]:
    return {
        "html_url": f"https://github.com/user/repo/pull/{number}",
        "number": str(number),
        "title": f"Pull {number}",
        "body": "",
        "base": {"sha": "base_sha", "ref": "main"},
        "head": {"sha": f"head_{number}_sha", "ref": f"branch-{number}"},
        "state": "open",
        "draft": False,
        "node_id": f"node_{number}",
        "merged_at": None,
        "merge_commit_sha": None,
        "mergeable": True,
        "mergeable_state": "clean",
    }


@pytest.fixture(autouse=True)
def _patch_retry_delay(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(approvals, "_MERGEABLE_RETRY_DELAY_SECONDS", 0.0)


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_fetch_approved_pull_numbers_empty_input(
    respx_mock: respx.MockRouter,  # noqa: ARG001
) -> None:
    async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
        result = await approvals.fetch_approved_pull_numbers(
            client,
            "user",
            "repo",
            [],
        )
    assert result == set()


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_fetch_approved_pull_numbers_single_approved(
    respx_mock: respx.MockRouter,
) -> None:
    respx_mock.get("/repos/user/repo/pulls/1/reviews").respond(
        200,
        json=[
            {"state": "APPROVED", "user": {"login": "alice"}},
        ],
    )
    async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
        result = await approvals.fetch_approved_pull_numbers(
            client,
            "user",
            "repo",
            [_pull(1)],  # type: ignore[list-item]
        )
    assert result == {1}


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_fetch_approved_pull_numbers_dismissed_overrides_approval(
    respx_mock: respx.MockRouter,
) -> None:
    respx_mock.get("/repos/user/repo/pulls/1/reviews").respond(
        200,
        json=[
            {"state": "APPROVED", "user": {"login": "alice"}},
            {"state": "DISMISSED", "user": {"login": "alice"}},
        ],
    )
    async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
        result = await approvals.fetch_approved_pull_numbers(
            client,
            "user",
            "repo",
            [_pull(1)],  # type: ignore[list-item]
        )
    assert result == set()


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_fetch_approved_pull_numbers_changes_requested_overrides_approval(
    respx_mock: respx.MockRouter,
) -> None:
    respx_mock.get("/repos/user/repo/pulls/1/reviews").respond(
        200,
        json=[
            {"state": "APPROVED", "user": {"login": "alice"}},
            {"state": "CHANGES_REQUESTED", "user": {"login": "alice"}},
        ],
    )
    async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
        result = await approvals.fetch_approved_pull_numbers(
            client,
            "user",
            "repo",
            [_pull(1)],  # type: ignore[list-item]
        )
    assert result == set()


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_fetch_approved_pull_numbers_commented_ignored_in_state_collapse(
    respx_mock: respx.MockRouter,
) -> None:
    respx_mock.get("/repos/user/repo/pulls/1/reviews").respond(
        200,
        json=[
            {"state": "APPROVED", "user": {"login": "alice"}},
            {"state": "COMMENTED", "user": {"login": "alice"}},
        ],
    )
    async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
        result = await approvals.fetch_approved_pull_numbers(
            client,
            "user",
            "repo",
            [_pull(1)],  # type: ignore[list-item]
        )
    assert result == {1}


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_fetch_approved_pull_numbers_multi_reviewer_any_approved(
    respx_mock: respx.MockRouter,
) -> None:
    respx_mock.get("/repos/user/repo/pulls/1/reviews").respond(
        200,
        json=[
            {"state": "CHANGES_REQUESTED", "user": {"login": "alice"}},
            {"state": "APPROVED", "user": {"login": "bob"}},
        ],
    )
    async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
        result = await approvals.fetch_approved_pull_numbers(
            client,
            "user",
            "repo",
            [_pull(1)],  # type: ignore[list-item]
        )
    assert result == {1}


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_fetch_approved_pull_numbers_multiple_pulls_parallel(
    respx_mock: respx.MockRouter,
) -> None:
    respx_mock.get("/repos/user/repo/pulls/1/reviews").respond(
        200,
        json=[{"state": "APPROVED", "user": {"login": "alice"}}],
    )
    respx_mock.get("/repos/user/repo/pulls/2/reviews").respond(
        200,
        json=[],
    )
    respx_mock.get("/repos/user/repo/pulls/3/reviews").respond(
        200,
        json=[{"state": "APPROVED", "user": {"login": "carol"}}],
    )
    async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
        result = await approvals.fetch_approved_pull_numbers(
            client,
            "user",
            "repo",
            [_pull(1), _pull(2), _pull(3)],  # type: ignore[list-item]
        )
    assert result == {1, 3}


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_fetch_approved_pull_numbers_pending_ignored_in_state_collapse(
    respx_mock: respx.MockRouter,
) -> None:
    respx_mock.get("/repos/user/repo/pulls/1/reviews").respond(
        200,
        json=[
            {"state": "APPROVED", "user": {"login": "alice"}},
            {"state": "PENDING", "user": {"login": "alice"}},
        ],
    )
    async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
        result = await approvals.fetch_approved_pull_numbers(
            client,
            "user",
            "repo",
            [_pull(1)],  # type: ignore[list-item]
        )
    assert result == {1}


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_fetch_approved_pull_numbers_http_error_propagates(
    respx_mock: respx.MockRouter,
) -> None:
    respx_mock.get("/repos/user/repo/pulls/1/reviews").respond(500)
    async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
        with pytest.raises(httpx.HTTPStatusError):
            await approvals.fetch_approved_pull_numbers(
                client,
                "user",
                "repo",
                [_pull(1)],  # type: ignore[list-item]
            )


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_bottom_pull_has_conflict_none_input(
    respx_mock: respx.MockRouter,  # noqa: ARG001
) -> None:
    async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
        assert (
            await approvals.bottom_pull_has_conflict(
                client,
                "user",
                "repo",
                None,
            )
            is False
        )


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_bottom_pull_has_conflict_dirty(
    respx_mock: respx.MockRouter,
) -> None:
    respx_mock.get("/repos/user/repo/pulls/1").respond(
        200,
        json={**_pull(1), "mergeable": False, "mergeable_state": "dirty"},
    )
    async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
        assert (
            await approvals.bottom_pull_has_conflict(
                client,
                "user",
                "repo",
                _pull(1),  # type: ignore[arg-type]
            )
            is True
        )


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_bottom_pull_has_conflict_behind_is_not_conflict(
    respx_mock: respx.MockRouter,
) -> None:
    respx_mock.get("/repos/user/repo/pulls/1").respond(
        200,
        json={**_pull(1), "mergeable": True, "mergeable_state": "behind"},
    )
    async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
        assert (
            await approvals.bottom_pull_has_conflict(
                client,
                "user",
                "repo",
                _pull(1),  # type: ignore[arg-type]
            )
            is False
        )


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_bottom_pull_has_conflict_clean(
    respx_mock: respx.MockRouter,
) -> None:
    respx_mock.get("/repos/user/repo/pulls/1").respond(
        200,
        json={**_pull(1), "mergeable": True, "mergeable_state": "clean"},
    )
    async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
        assert (
            await approvals.bottom_pull_has_conflict(
                client,
                "user",
                "repo",
                _pull(1),  # type: ignore[arg-type]
            )
            is False
        )


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_bottom_pull_has_conflict_null_then_resolves(
    respx_mock: respx.MockRouter,
) -> None:
    route = respx_mock.get("/repos/user/repo/pulls/1")
    route.side_effect = [
        httpx.Response(
            200,
            json={**_pull(1), "mergeable": None, "mergeable_state": "unknown"},
        ),
        httpx.Response(
            200,
            json={**_pull(1), "mergeable": False, "mergeable_state": "dirty"},
        ),
    ]
    async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
        assert (
            await approvals.bottom_pull_has_conflict(
                client,
                "user",
                "repo",
                _pull(1),  # type: ignore[arg-type]
            )
            is True
        )


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_bottom_pull_has_conflict_stays_null(
    respx_mock: respx.MockRouter,
) -> None:
    respx_mock.get("/repos/user/repo/pulls/1").respond(
        200,
        json={**_pull(1), "mergeable": None, "mergeable_state": "unknown"},
    )
    async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
        assert (
            await approvals.bottom_pull_has_conflict(
                client,
                "user",
                "repo",
                _pull(1),  # type: ignore[arg-type]
            )
            is False
        )


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_bottom_pull_has_conflict_http_error_is_non_conflict(
    respx_mock: respx.MockRouter,
) -> None:
    respx_mock.get("/repos/user/repo/pulls/1").respond(500)
    async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
        assert (
            await approvals.bottom_pull_has_conflict(
                client,
                "user",
                "repo",
                _pull(1),  # type: ignore[arg-type]
            )
            is False
        )


def _local_change(
    change_id: str,
    pull: dict[str, object] | None,
    *,
    action: str,
    base_branch: str = "main",
    dest_branch: str = "branch",
) -> changes.LocalChange:
    return changes.LocalChange(
        id=changes.ChangeId(change_id),
        pull=pull,  # type: ignore[arg-type]
        commit_sha="abc",
        title="T",
        message="M",
        base_branch=base_branch,
        dest_branch=dest_branch,
        action=action,  # type: ignore[arg-type]
    )


def _planned_changes(locals_: list[changes.LocalChange]) -> changes.Changes:
    return changes.Changes(stack_prefix="", locals=locals_)


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_decide_rebase_skip_rebase_flag_wins(
    respx_mock: respx.MockRouter,  # noqa: ARG001
) -> None:
    planned = _planned_changes(
        [_local_change("Iaa", _pull(1), action="update")],
    )
    with (
        mock.patch.object(
            approvals,
            "fetch_approved_pull_numbers",
        ) as fetch_mock,
        mock.patch.object(
            approvals,
            "bottom_pull_has_conflict",
        ) as conflict_mock,
    ):
        async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
            decision = await approvals.decide_rebase(
                client,
                "user",
                "repo",
                planned_changes=planned,
                skip_rebase=True,
                force_rebase=False,
            )
    assert decision.should_rebase is False
    assert decision.reason is approvals.RebaseReason.EXPLICIT_SKIP
    fetch_mock.assert_not_called()
    conflict_mock.assert_not_called()


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_decide_rebase_force_flag_wins(
    respx_mock: respx.MockRouter,  # noqa: ARG001
) -> None:
    planned = _planned_changes(
        [_local_change("Iaa", _pull(1), action="update")],
    )
    with (
        mock.patch.object(
            approvals,
            "fetch_approved_pull_numbers",
        ) as fetch_mock,
        mock.patch.object(
            approvals,
            "bottom_pull_has_conflict",
        ) as conflict_mock,
    ):
        async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
            decision = await approvals.decide_rebase(
                client,
                "user",
                "repo",
                planned_changes=planned,
                skip_rebase=False,
                force_rebase=True,
            )
    assert decision.should_rebase is True
    assert decision.reason is approvals.RebaseReason.FORCED
    fetch_mock.assert_not_called()
    conflict_mock.assert_not_called()


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_decide_rebase_no_approvals_no_conflict_rebases(
    respx_mock: respx.MockRouter,  # noqa: ARG001
) -> None:
    planned = _planned_changes(
        [_local_change("Iaa", _pull(1), action="update")],
    )
    with (
        mock.patch.object(
            approvals,
            "fetch_approved_pull_numbers",
            return_value=set(),
        ),
        mock.patch.object(
            approvals,
            "bottom_pull_has_conflict",
            return_value=False,
        ),
    ):
        async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
            decision = await approvals.decide_rebase(
                client,
                "user",
                "repo",
                planned_changes=planned,
                skip_rebase=False,
                force_rebase=False,
            )
    assert decision.should_rebase is True
    assert decision.reason is approvals.RebaseReason.NO_APPROVALS
    assert decision.approved_pulls == []


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_decide_rebase_approvals_clean_bottom_skips(
    respx_mock: respx.MockRouter,  # noqa: ARG001
) -> None:
    pull1 = _pull(1)
    planned = _planned_changes(
        [_local_change("Iaa", pull1, action="update")],
    )
    with (
        mock.patch.object(
            approvals,
            "fetch_approved_pull_numbers",
            return_value={1},
        ),
        mock.patch.object(
            approvals,
            "bottom_pull_has_conflict",
            return_value=False,
        ),
    ):
        async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
            decision = await approvals.decide_rebase(
                client,
                "user",
                "repo",
                planned_changes=planned,
                skip_rebase=False,
                force_rebase=False,
            )
    assert decision.should_rebase is False
    assert decision.reason is approvals.RebaseReason.SKIPPED_FOR_APPROVALS
    assert [p["number"] for p in decision.approved_pulls] == [pull1["number"]]


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_decide_rebase_approvals_with_dirty_bottom_overrides(
    respx_mock: respx.MockRouter,  # noqa: ARG001
) -> None:
    pull1 = _pull(1)
    planned = _planned_changes(
        [_local_change("Iaa", pull1, action="update")],
    )
    with (
        mock.patch.object(
            approvals,
            "fetch_approved_pull_numbers",
            return_value={1},
        ),
        mock.patch.object(
            approvals,
            "bottom_pull_has_conflict",
            return_value=True,
        ),
    ):
        async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
            decision = await approvals.decide_rebase(
                client,
                "user",
                "repo",
                planned_changes=planned,
                skip_rebase=False,
                force_rebase=False,
            )
    assert decision.should_rebase is True
    assert decision.reason is approvals.RebaseReason.CONFLICT_OVERRIDE
    assert [p["number"] for p in decision.approved_pulls] == [pull1["number"]]


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_decide_rebase_no_approvals_dirty_bottom_rebases_no_approvals_reason(
    respx_mock: respx.MockRouter,  # noqa: ARG001
) -> None:
    planned = _planned_changes(
        [_local_change("Iaa", _pull(1), action="update")],
    )
    with (
        mock.patch.object(
            approvals,
            "fetch_approved_pull_numbers",
            return_value=set(),
        ),
        mock.patch.object(
            approvals,
            "bottom_pull_has_conflict",
            return_value=True,
        ),
    ):
        async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
            decision = await approvals.decide_rebase(
                client,
                "user",
                "repo",
                planned_changes=planned,
                skip_rebase=False,
                force_rebase=False,
            )
    assert decision.should_rebase is True
    assert decision.reason is approvals.RebaseReason.NO_APPROVALS


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_decide_rebase_ignores_create_action_pulls(
    respx_mock: respx.MockRouter,  # noqa: ARG001
) -> None:
    # Mixed stack: a create (no pull) and an update (with pull).
    pull2 = _pull(2)
    planned = _planned_changes(
        [
            _local_change("Iaa", None, action="create"),
            _local_change("Ibb", pull2, action="update"),
        ],
    )
    with (
        mock.patch.object(
            approvals,
            "fetch_approved_pull_numbers",
            return_value=set(),
        ) as fetch_mock,
        mock.patch.object(
            approvals,
            "bottom_pull_has_conflict",
            return_value=False,
        ) as conflict_mock,
    ):
        async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
            await approvals.decide_rebase(
                client,
                "user",
                "repo",
                planned_changes=planned,
                skip_rebase=False,
                force_rebase=False,
            )
    # fetch receives only the update's pull
    fetched_pulls = fetch_mock.call_args.args[3]
    assert [p["number"] for p in fetched_pulls] == [pull2["number"]]
    # bottom conflict check sees None (bottom is a create with no pull)
    assert conflict_mock.call_args.args[3] is None


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_decide_rebase_skip_merged_bottom_resolves_to_next_live_pull(
    respx_mock: respx.MockRouter,  # noqa: ARG001
) -> None:
    # locals[0] is skip-merged (already merged PR). The "bottom" for conflict
    # purposes must resolve to the next live change — not be silently skipped.
    pull1 = _pull(1)
    pull2 = _pull(2)
    planned = _planned_changes(
        [
            _local_change("Iaa", pull1, action="skip-merged"),
            _local_change("Ibb", pull2, action="update"),
        ],
    )
    with (
        mock.patch.object(
            approvals,
            "fetch_approved_pull_numbers",
            return_value=set(),
        ) as fetch_mock,
        mock.patch.object(
            approvals,
            "bottom_pull_has_conflict",
            return_value=False,
        ) as conflict_mock,
    ):
        async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
            await approvals.decide_rebase(
                client,
                "user",
                "repo",
                planned_changes=planned,
                skip_rebase=False,
                force_rebase=False,
            )
    # Approval check excludes the merged pull but includes the live update.
    fetched_pulls = fetch_mock.call_args.args[3]
    assert [p["number"] for p in fetched_pulls] == [pull2["number"]]
    # Bottom conflict check resolves to pull2, not pull1 (merged) and not None.
    assert conflict_mock.call_args.args[3] is pull2


@pytest.mark.respx(base_url="https://api.github.com/")
async def test_decide_rebase_checks_skip_up_to_date_pulls(
    respx_mock: respx.MockRouter,  # noqa: ARG001
) -> None:
    # `skip-up-to-date` PRs get promoted to `update` when the stack is
    # rebased, so their approvals must be considered too.
    pull1 = _pull(1)
    planned = _planned_changes(
        [_local_change("Iaa", pull1, action="skip-up-to-date")],
    )
    with (
        mock.patch.object(
            approvals,
            "fetch_approved_pull_numbers",
            return_value={1},
        ) as fetch_mock,
        mock.patch.object(
            approvals,
            "bottom_pull_has_conflict",
            return_value=False,
        ) as conflict_mock,
    ):
        async with httpx.AsyncClient(base_url="https://api.github.com/") as client:
            decision = await approvals.decide_rebase(
                client,
                "user",
                "repo",
                planned_changes=planned,
                skip_rebase=False,
                force_rebase=False,
            )
    # The skip-up-to-date pull is included in both checks.
    fetched_pulls = fetch_mock.call_args.args[3]
    assert [p["number"] for p in fetched_pulls] == [pull1["number"]]
    assert conflict_mock.call_args.args[3] is pull1
    assert decision.should_rebase is False
    assert decision.reason is approvals.RebaseReason.SKIPPED_FOR_APPROVALS
