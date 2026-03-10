from __future__ import annotations

import asyncio
import webbrowser

import click

from mergify_cli import console
from mergify_cli import utils
from mergify_cli.reviews import api as reviews_api


@click.command(
    help="List PRs awaiting your review. Only shows PRs targeting the default branch that you have not already approved.",
)
@click.option(
    "--token",
    "-t",
    help="GitHub token",
    envvar="GITHUB_TOKEN",
    required=True,
    default=lambda: asyncio.run(utils.get_default_token()),
)
@click.option(
    "--github-server",
    default="https://api.github.com",
    envvar="GITHUB_API_URL",
    help="GitHub API URL",
    show_default=True,
)
@click.option(
    "--query",
    "-q",
    default="draft:false is:open is:pr review-requested:@me sort:updated-desc",
    show_default=True,
    help="GitHub search query for filtering PRs",
)
@click.option(
    "--browse",
    "-b",
    is_flag=True,
    default=False,
    help="Open all listed PRs in the browser",
)
@utils.run_with_asyncio
async def reviews(
    *,
    token: str,
    github_server: str,
    query: str,
    browse: bool,
) -> None:
    async with utils.get_github_http_client(github_server, token) as client:
        pull_requests_by_repository = (
            await reviews_api.get_default_branch_pending_reviews(
                client=client,
                login=await reviews_api.get_user_login(client),
                query=query,
            )
        )

    if not pull_requests_by_repository:
        console.print("No PRs awaiting your review.")
        return

    for repository, pull_requests in pull_requests_by_repository.items():
        console.print(f"[bold]{repository}[/]")

        for pull_request in pull_requests:
            console.print(
                f"  [link={pull_request['url']}]#{pull_request['number']} {pull_request['title']}[/link]"
                f"{f' by {pull_request["author"]}' if pull_request['author'] else ''}",
            )

            if browse:
                webbrowser.open(pull_request["url"])
