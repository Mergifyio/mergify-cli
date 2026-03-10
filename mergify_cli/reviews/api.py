from __future__ import annotations

import typing


if typing.TYPE_CHECKING:
    import httpx


_PENDING_REVIEWS_QUERY = """
query($author_login: String!, $search_query: String!) {
  search(query: $search_query, type: ISSUE, first: 50) {
    nodes {
      ... on PullRequest {
        number
        title
        url
        baseRefName
        author { login }
        repository { nameWithOwner defaultBranchRef { name } }
        reviews(author: $author_login, last: 1, states: [APPROVED]) {
          totalCount
        }
      }
    }
  }
}
"""


class _GraphQLError(Exception):
    """Raised when the GitHub GraphQL API returns query-level errors."""


class _PullRequest(typing.TypedDict):
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
        # Filter out approved PRs and PRs not targeting the default branch.
        if (
            not node
            or node["reviews"]["totalCount"] > 0
            or node["baseRefName"] != node["repository"]["defaultBranchRef"]["name"]
        ):
            continue

        result.append(
            _PullRequest(
                repository=node["repository"]["nameWithOwner"],
                number=node["number"],
                title=node["title"],
                url=node["url"],
                author=node["author"]["login"] if node["author"] else None,
            ),
        )

    return result


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

    return _group_pull_requests_by_repository(
        pull_requests=_parse_default_branch_pull_requests(data),
    )
