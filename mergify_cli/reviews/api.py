from __future__ import annotations

import typing


if typing.TYPE_CHECKING:
    import httpx


_PENDING_REVIEWS_QUERY = """
query($author_login: String!, $search_query: String!) {
  search(query: $search_query, type: ISSUE, first: 50) {
    nodes {
      ... on PullRequest {
        id
        number
        title
        url
        baseRefName
        author { login }
        repository { nameWithOwner defaultBranchRef { name } }
        mergeable
        reviews(author: $author_login, last: 1, states: [APPROVED]) {
          totalCount
        }
      }
    }
  }
}
"""

_REQUIRED_CHECKS_PR_FRAGMENT = """
  pr_{index}: node(id: "{node_id}") {{
    ... on PullRequest {{
      commits(last: 1) {{
        nodes {{
          commit {{
            statusCheckRollup {{
              contexts(first: 100) {{
                nodes {{
                  ... on CheckRun {{
                    conclusion
                    isRequired(pullRequestId: "{node_id}")
                  }}
                  ... on StatusContext {{
                    state
                    isRequired(pullRequestId: "{node_id}")
                  }}
                }}
              }}
            }}
          }}
        }}
      }}
    }}
  }}
"""


class _GraphQLError(Exception):
    """Raised when the GitHub GraphQL API returns query-level errors."""


class _PullRequest(typing.TypedDict):
    node_id: str
    repository: str
    number: int
    title: str
    url: str
    author: str | None


async def get_user_login(client: httpx.AsyncClient) -> str:
    response = await client.get("/user")
    response.raise_for_status()

    return str(response.json()["login"])


def _raise_on_graphql_errors(data: dict[str, typing.Any]) -> None:
    if errors := data.get("errors"):
        raise _GraphQLError(
            "; ".join(error.get("message", str(error)) for error in errors),
        )

    if data.get("data") is None:
        raise _GraphQLError("GraphQL response contains no data")


def _parse_default_branch_pull_requests(
    data: dict[str, typing.Any],
) -> list[_PullRequest]:
    result: list[_PullRequest] = []
    for node in data["data"]["search"]["nodes"]:
        # Filter out approved PRs, conflicting PRs, and PRs not targeting
        # the default branch.
        if (
            not node
            or node["reviews"]["totalCount"] > 0
            or node["mergeable"] == "CONFLICTING"
            or node["baseRefName"] != node["repository"]["defaultBranchRef"]["name"]
        ):
            continue

        result.append(
            _PullRequest(
                node_id=node["id"],
                repository=node["repository"]["nameWithOwner"],
                number=node["number"],
                title=node["title"],
                url=node["url"],
                author=node["author"]["login"] if node["author"] else None,
            ),
        )

    return result


_FAILING_CHECK_RUN_CONCLUSIONS = frozenset(
    {
        "ACTION_REQUIRED",
        "CANCELLED",
        "FAILURE",
        "STARTUP_FAILURE",
        "TIMED_OUT",
    },
)

_FAILING_STATUS_CONTEXT_STATES = frozenset({"ERROR", "FAILURE"})


def _has_failing_required_checks(pr_node: dict[str, typing.Any]) -> bool:
    commits = pr_node.get("commits", {}).get("nodes") or []
    if not commits:
        return False

    rollup = commits[0].get("commit", {}).get("statusCheckRollup")
    if rollup is None:
        return False

    for context in rollup.get("contexts", {}).get("nodes") or []:
        if not context.get("isRequired"):
            continue

        # CheckRun
        if "conclusion" in context:
            if context["conclusion"] in _FAILING_CHECK_RUN_CONCLUSIONS:
                return True
        # StatusContext
        elif "state" in context and context["state"] in _FAILING_STATUS_CONTEXT_STATES:
            return True

    return False


async def _get_pr_ids_with_failing_required_checks(
    client: httpx.AsyncClient,
    pull_requests: list[_PullRequest],
) -> set[str]:
    """Return node IDs of PRs that have at least one failing required check."""
    if not pull_requests:
        return set()

    fragments = [
        _REQUIRED_CHECKS_PR_FRAGMENT.format(index=i, node_id=pr["node_id"])
        for i, pr in enumerate(pull_requests)
    ]
    query = "query {" + "".join(fragments) + "\n}"

    response = await client.post("/graphql", json={"query": query})
    response.raise_for_status()

    data = response.json()
    _raise_on_graphql_errors(data)

    failing: set[str] = set()
    for i, pr in enumerate(pull_requests):
        pr_data = data["data"].get(f"pr_{i}")
        if not pr_data or _has_failing_required_checks(pr_data):
            failing.add(pr["node_id"])

    return failing


def _group_pull_requests_by_repository(
    pull_requests: list[_PullRequest],
) -> dict[str, list[_PullRequest]]:
    result: dict[str, list[_PullRequest]] = {}
    for pull_request in pull_requests:
        result.setdefault(pull_request["repository"], []).append(pull_request)

    return result


async def get_default_branch_pending_reviews(
    client: httpx.AsyncClient,
    login: str,
    query: str,
) -> dict[str, list[_PullRequest]]:
    """Fetch PRs awaiting review, grouped by repository."""
    response = await client.post(
        "/graphql",
        json={
            "query": _PENDING_REVIEWS_QUERY,
            "variables": {"author_login": login, "search_query": query},
        },
    )
    response.raise_for_status()

    data = response.json()
    _raise_on_graphql_errors(data)

    pull_requests = _parse_default_branch_pull_requests(data)

    failing_ids = await _get_pr_ids_with_failing_required_checks(
        client,
        pull_requests,
    )

    return _group_pull_requests_by_repository(
        pull_requests=[pr for pr in pull_requests if pr["node_id"] not in failing_ids],
    )
