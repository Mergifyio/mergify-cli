from __future__ import annotations

from unittest import mock

from click.testing import CliRunner
from httpx import Response
import respx

from mergify_cli.reviews.cli import reviews


def _make_no_failing_checks_response(count: int) -> dict[str, object]:
    """Build a required-checks GraphQL response where no required check fails."""
    return {
        "data": {
            f"pr_{i}": {
                "commits": {
                    "nodes": [
                        {"commit": {"statusCheckRollup": None}},
                    ],
                },
            }
            for i in range(count)
        },
    }


_GRAPHQL_RESPONSE_EMPTY: dict[str, object] = {
    "data": {"search": {"nodes": []}},
}

_GRAPHQL_RESPONSE_WITH_PRS: dict[str, object] = {
    "data": {
        "search": {
            "nodes": [
                {
                    "id": "PR_42",
                    "number": 42,
                    "title": "Add feature X",
                    "url": "https://github.com/owner/repo/pull/42",
                    "baseRefName": "main",
                    "author": {"login": "alice"},
                    "repository": {
                        "nameWithOwner": "owner/repo",
                        "defaultBranchRef": {"name": "main"},
                    },
                    "mergeable": "MERGEABLE",
                    "reviews": {"totalCount": 0},
                },
                {
                    "id": "PR_99",
                    "number": 99,
                    "title": "Fix bug Y",
                    "url": "https://github.com/org/other/pull/99",
                    "baseRefName": "main",
                    "author": {"login": "bob"},
                    "repository": {
                        "nameWithOwner": "org/other",
                        "defaultBranchRef": {"name": "main"},
                    },
                    "mergeable": "MERGEABLE",
                    "reviews": {"totalCount": 0},
                },
            ],
        },
    },
}

_GRAPHQL_RESPONSE_ALREADY_APPROVED: dict[str, object] = {
    "data": {
        "search": {
            "nodes": [
                {
                    "id": "PR_10",
                    "number": 10,
                    "title": "Already approved",
                    "url": "https://github.com/owner/repo/pull/10",
                    "baseRefName": "main",
                    "author": {"login": "carol"},
                    "repository": {
                        "nameWithOwner": "owner/repo",
                        "defaultBranchRef": {"name": "main"},
                    },
                    "mergeable": "MERGEABLE",
                    "reviews": {"totalCount": 1},
                },
            ],
        },
    },
}

_GRAPHQL_RESPONSE_NON_DEFAULT_BRANCH: dict[str, object] = {
    "data": {
        "search": {
            "nodes": [
                {
                    "id": "PR_20",
                    "number": 20,
                    "title": "Feature branch PR",
                    "url": "https://github.com/owner/repo/pull/20",
                    "baseRefName": "develop",
                    "author": {"login": "dave"},
                    "repository": {
                        "nameWithOwner": "owner/repo",
                        "defaultBranchRef": {"name": "main"},
                    },
                    "mergeable": "MERGEABLE",
                    "reviews": {"totalCount": 0},
                },
            ],
        },
    },
}

_GRAPHQL_RESPONSE_CONFLICTING: dict[str, object] = {
    "data": {
        "search": {
            "nodes": [
                {
                    "id": "PR_30",
                    "number": 30,
                    "title": "Conflicting PR",
                    "url": "https://github.com/owner/repo/pull/30",
                    "baseRefName": "main",
                    "author": {"login": "eve"},
                    "repository": {
                        "nameWithOwner": "owner/repo",
                        "defaultBranchRef": {"name": "main"},
                    },
                    "mergeable": "CONFLICTING",
                    "reviews": {"totalCount": 0},
                },
            ],
        },
    },
}

_USER_RESPONSE = {"login": "testuser"}

_BASE_ARGS = [
    "--token",
    "test-token",
    "--github-server",
    "https://api.github.com",
]


def test_no_pending_reviews() -> None:
    with respx.mock(base_url="https://api.github.com") as rsp:
        rsp.get("/user").mock(return_value=Response(200, json=_USER_RESPONSE))
        rsp.post("/graphql").mock(
            return_value=Response(200, json=_GRAPHQL_RESPONSE_EMPTY),
        )

        result = CliRunner().invoke(reviews, _BASE_ARGS)
        assert result.exit_code == 0
        assert result.output == "No PRs awaiting your review.\n"


def test_pending_reviews() -> None:
    with respx.mock(base_url="https://api.github.com") as rsp:
        rsp.get("/user").mock(return_value=Response(200, json=_USER_RESPONSE))
        rsp.post("/graphql").mock(
            side_effect=[
                Response(200, json=_GRAPHQL_RESPONSE_WITH_PRS),
                Response(200, json=_make_no_failing_checks_response(2)),
            ],
        )

        result = CliRunner().invoke(reviews, _BASE_ARGS)
        assert result.exit_code == 0, result.output
        assert (
            result.output
            == """owner/repo
  #42 Add feature X by alice
org/other
  #99 Fix bug Y by bob
"""
        )


def test_filters_approved_prs() -> None:
    with respx.mock(base_url="https://api.github.com") as rsp:
        rsp.get("/user").mock(return_value=Response(200, json=_USER_RESPONSE))
        rsp.post("/graphql").mock(
            return_value=Response(200, json=_GRAPHQL_RESPONSE_ALREADY_APPROVED),
        )

        result = CliRunner().invoke(reviews, _BASE_ARGS)
        assert result.exit_code == 0, result.output
        assert result.output == "No PRs awaiting your review.\n"


def test_filters_non_default_branch_prs() -> None:
    with respx.mock(base_url="https://api.github.com") as rsp:
        rsp.get("/user").mock(return_value=Response(200, json=_USER_RESPONSE))
        rsp.post("/graphql").mock(
            return_value=Response(200, json=_GRAPHQL_RESPONSE_NON_DEFAULT_BRANCH),
        )

        result = CliRunner().invoke(reviews, _BASE_ARGS)
        assert result.exit_code == 0, result.output
        assert result.output == "No PRs awaiting your review.\n"


def test_browse_opens_urls() -> None:
    with respx.mock(base_url="https://api.github.com") as rsp:
        rsp.get("/user").mock(return_value=Response(200, json=_USER_RESPONSE))
        rsp.post("/graphql").mock(
            side_effect=[
                Response(200, json=_GRAPHQL_RESPONSE_WITH_PRS),
                Response(200, json=_make_no_failing_checks_response(2)),
            ],
        )

        with mock.patch("mergify_cli.reviews.cli.webbrowser.open") as mock_open:
            result = CliRunner().invoke(reviews, [*_BASE_ARGS, "--browse"])
            assert result.exit_code == 0, result.output
            assert mock_open.call_count == 2
            mock_open.assert_any_call(
                "https://github.com/owner/repo/pull/42",
            )
            mock_open.assert_any_call(
                "https://github.com/org/other/pull/99",
            )


def test_null_author_hidden() -> None:
    graphql_response: dict[str, object] = {
        "data": {
            "search": {
                "nodes": [
                    {
                        "id": "PR_7",
                        "number": 7,
                        "title": "Ghost PR",
                        "url": "https://github.com/owner/repo/pull/7",
                        "baseRefName": "main",
                        "author": None,
                        "repository": {
                            "nameWithOwner": "owner/repo",
                            "defaultBranchRef": {"name": "main"},
                        },
                        "mergeable": "MERGEABLE",
                        "reviews": {"totalCount": 0},
                    },
                ],
            },
        },
    }

    with respx.mock(base_url="https://api.github.com") as rsp:
        rsp.get("/user").mock(return_value=Response(200, json=_USER_RESPONSE))
        rsp.post("/graphql").mock(
            side_effect=[
                Response(200, json=graphql_response),
                Response(200, json=_make_no_failing_checks_response(1)),
            ],
        )

        result = CliRunner().invoke(reviews, _BASE_ARGS)
        assert result.exit_code == 0, result.output
        assert (
            result.output
            == """owner/repo
  #7 Ghost PR
"""
        )


def test_filters_conflicting_prs() -> None:
    with respx.mock(base_url="https://api.github.com") as rsp:
        rsp.get("/user").mock(return_value=Response(200, json=_USER_RESPONSE))
        rsp.post("/graphql").mock(
            return_value=Response(200, json=_GRAPHQL_RESPONSE_CONFLICTING),
        )

        result = CliRunner().invoke(reviews, _BASE_ARGS)
        assert result.exit_code == 0, result.output
        assert result.output == "No PRs awaiting your review.\n"


def test_filters_prs_with_failing_required_checks() -> None:
    search_response: dict[str, object] = {
        "data": {
            "search": {
                "nodes": [
                    {
                        "id": "PR_50",
                        "number": 50,
                        "title": "Failing required checks",
                        "url": "https://github.com/owner/repo/pull/50",
                        "baseRefName": "main",
                        "author": {"login": "frank"},
                        "repository": {
                            "nameWithOwner": "owner/repo",
                            "defaultBranchRef": {"name": "main"},
                        },
                        "mergeable": "MERGEABLE",
                        "reviews": {"totalCount": 0},
                    },
                ],
            },
        },
    }
    checks_response: dict[str, object] = {
        "data": {
            "pr_0": {
                "commits": {
                    "nodes": [
                        {
                            "commit": {
                                "statusCheckRollup": {
                                    "contexts": {
                                        "nodes": [
                                            {
                                                "conclusion": "FAILURE",
                                                "isRequired": True,
                                            },
                                        ],
                                    },
                                },
                            },
                        },
                    ],
                },
            },
        },
    }

    with respx.mock(base_url="https://api.github.com") as rsp:
        rsp.get("/user").mock(return_value=Response(200, json=_USER_RESPONSE))
        rsp.post("/graphql").mock(
            side_effect=[
                Response(200, json=search_response),
                Response(200, json=checks_response),
            ],
        )

        result = CliRunner().invoke(reviews, _BASE_ARGS)
        assert result.exit_code == 0, result.output
        assert result.output == "No PRs awaiting your review.\n"


def test_filters_prs_with_failing_required_status_context() -> None:
    search_response: dict[str, object] = {
        "data": {
            "search": {
                "nodes": [
                    {
                        "id": "PR_55",
                        "number": 55,
                        "title": "Failing required status",
                        "url": "https://github.com/owner/repo/pull/55",
                        "baseRefName": "main",
                        "author": {"login": "hank"},
                        "repository": {
                            "nameWithOwner": "owner/repo",
                            "defaultBranchRef": {"name": "main"},
                        },
                        "mergeable": "MERGEABLE",
                        "reviews": {"totalCount": 0},
                    },
                ],
            },
        },
    }
    checks_response: dict[str, object] = {
        "data": {
            "pr_0": {
                "commits": {
                    "nodes": [
                        {
                            "commit": {
                                "statusCheckRollup": {
                                    "contexts": {
                                        "nodes": [
                                            {
                                                "state": "FAILURE",
                                                "isRequired": True,
                                            },
                                        ],
                                    },
                                },
                            },
                        },
                    ],
                },
            },
        },
    }

    with respx.mock(base_url="https://api.github.com") as rsp:
        rsp.get("/user").mock(return_value=Response(200, json=_USER_RESPONSE))
        rsp.post("/graphql").mock(
            side_effect=[
                Response(200, json=search_response),
                Response(200, json=checks_response),
            ],
        )

        result = CliRunner().invoke(reviews, _BASE_ARGS)
        assert result.exit_code == 0, result.output
        assert result.output == "No PRs awaiting your review.\n"


def test_keeps_prs_with_failing_non_required_checks() -> None:
    search_response: dict[str, object] = {
        "data": {
            "search": {
                "nodes": [
                    {
                        "id": "PR_60",
                        "number": 60,
                        "title": "Non-required check failing",
                        "url": "https://github.com/owner/repo/pull/60",
                        "baseRefName": "main",
                        "author": {"login": "grace"},
                        "repository": {
                            "nameWithOwner": "owner/repo",
                            "defaultBranchRef": {"name": "main"},
                        },
                        "mergeable": "MERGEABLE",
                        "reviews": {"totalCount": 0},
                    },
                ],
            },
        },
    }
    checks_response: dict[str, object] = {
        "data": {
            "pr_0": {
                "commits": {
                    "nodes": [
                        {
                            "commit": {
                                "statusCheckRollup": {
                                    "contexts": {
                                        "nodes": [
                                            {
                                                "conclusion": "SUCCESS",
                                                "isRequired": True,
                                            },
                                            {
                                                "conclusion": "FAILURE",
                                                "isRequired": False,
                                            },
                                        ],
                                    },
                                },
                            },
                        },
                    ],
                },
            },
        },
    }

    with respx.mock(base_url="https://api.github.com") as rsp:
        rsp.get("/user").mock(return_value=Response(200, json=_USER_RESPONSE))
        rsp.post("/graphql").mock(
            side_effect=[
                Response(200, json=search_response),
                Response(200, json=checks_response),
            ],
        )

        result = CliRunner().invoke(reviews, _BASE_ARGS)
        assert result.exit_code == 0, result.output
        assert (
            result.output
            == """owner/repo
  #60 Non-required check failing by grace
"""
        )


def test_graphql_errors() -> None:
    with respx.mock(base_url="https://api.github.com") as rsp:
        rsp.get("/user").mock(return_value=Response(200, json=_USER_RESPONSE))
        rsp.post("/graphql").mock(
            return_value=Response(
                200,
                json={"errors": [{"message": "rate limited"}], "data": None},
            ),
        )

        result = CliRunner().invoke(reviews, _BASE_ARGS)
        assert result.exit_code != 0


def test_graphql_null_data() -> None:
    with respx.mock(base_url="https://api.github.com") as rsp:
        rsp.get("/user").mock(return_value=Response(200, json=_USER_RESPONSE))
        rsp.post("/graphql").mock(
            return_value=Response(200, json={"data": None}),
        )

        result = CliRunner().invoke(reviews, _BASE_ARGS)
        assert result.exit_code != 0


def test_api_error() -> None:
    with respx.mock(base_url="https://api.github.com") as rsp:
        rsp.get("/user").mock(
            return_value=Response(401, json={"message": "Bad credentials"}),
        )

        result = CliRunner().invoke(reviews, _BASE_ARGS)
        assert result.exit_code != 0
