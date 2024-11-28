import asyncio
import json
import os
import pathlib
import re
import typing
from urllib import parse

import click

from mergify_cli import console
from mergify_cli import utils
from mergify_cli.ci import junit_upload as junit_upload_mod


ci = click.Group(
    "ci",
    help="Mergify's CI related commands",
)


CIProviderT = typing.Literal["github_action", "circleci"]


def get_ci_provider() -> CIProviderT | None:
    if os.getenv("GITHUB_ACTIONS") == "true":
        return "github_action"
    if os.getenv("CIRCLECI") == "true":
        return "circleci"
    return None


def get_job_name() -> str | None:
    if get_ci_provider() == "github_action":
        return os.getenv("GITHUB_WORKFLOW")
    if get_ci_provider() == "circleci":
        return os.getenv("CIRCLE_JOB")

    console.log("Error: failed to get the job's name from env", style="red")
    return None


def get_github_actions_head_sha() -> str | None:
    if os.getenv("GITHUB_EVENT_NAME") == "pull_request":
        # NOTE(leo): we want the head sha of pull request
        event_raw_path = os.getenv("GITHUB_EVENT_PATH")
        if event_raw_path and ((event_path := pathlib.Path(event_raw_path)).is_file()):
            event = json.loads(event_path.read_bytes())
            return str(event["pull_request"]["head"]["sha"])
    return os.getenv("GITHUB_SHA")


async def get_circle_ci_head_sha() -> str | None:
    if (pull_url := os.getenv("CIRCLE_PULL_REQUESTS")) and len(
        pull_url.split(","),
    ) == 1:
        if not (token := os.getenv("GITHUB_TOKEN")):
            msg = (
                "Failed to detect the head sha of the pull request associated"
                " to this run. Please make sure to set a token in the env "
                "variable 'GITHUB_TOKEN' for this purpose."
            )
            raise RuntimeError(msg)

        parsed_url = parse.urlparse(pull_url)
        if parsed_url.netloc == "github.com":
            github_server = "https://api.github.com"
        else:
            github_server = f"{parsed_url.scheme}://{parsed_url.netloc}/api/v3"

        async with utils.get_github_http_client(github_server, token) as client:
            resp = await client.get(f"/repos{parsed_url.path}")

        return str(resp.json()["head"]["sha"])

    return os.getenv("CIRCLE_SHA1")


async def get_head_sha() -> str | None:
    if get_ci_provider() == "github_action":
        return get_github_actions_head_sha()
    if get_ci_provider() == "circleci":
        return await get_circle_ci_head_sha()

    console.log("Error: failed to get the head SHA from env", style="red")
    return None


def get_github_repository() -> str | None:
    if get_ci_provider() == "github_action":
        return os.getenv("GITHUB_REPOSITORY")
    if get_ci_provider() == "circleci":
        repository_url = os.getenv("CIRCLE_REPOSITORY_URL")
        if repository_url and (
            match := re.match(
                r"(https?://[\w.-]+/)?(?P<full_name>[\w.-]+/[\w.-]+)/?$",
                repository_url,
            )
        ):
            return match.group("full_name")

    console.log("Error: failed to get the GitHub repository from env", style="red")
    return None


@ci.command(help="Upload JUnit XML reports")
@click.option(
    "--api-url",
    "-u",
    help="URL of the Mergify API",
    required=True,
    envvar="MERGIFY_API_URL",
    default="https://api.mergify.com",
    show_default=True,
)
@click.option(
    "--token",
    "-t",
    help="CI Issues Application Key",
    required=True,
    envvar="MERGIFY_TOKEN",
)
@click.option(
    "--repository",
    "-r",
    help="Repository full name (owner/repo)",
    required=True,
    default=get_github_repository,
)
@click.option(
    "--head-sha",
    "-s",
    help="Head SHA of the triggered job",
    required=True,
    default=lambda: asyncio.run(get_head_sha()),
)
@click.option(
    "--job-name",
    "-j",
    help="Job's name",
    required=True,
    default=get_job_name,
)
@click.option(
    "--provider",
    "-p",
    help="CI provider",
    default=get_ci_provider,
)
@click.argument(
    "files",
    nargs=-1,
    required=True,
    type=click.Path(exists=True, dir_okay=False),
)
@utils.run_with_asyncio
async def junit_upload(  # noqa: PLR0913, PLR0917
    api_url: str,
    token: str,
    repository: str,
    head_sha: str,
    job_name: str,
    provider: str | None,
    files: tuple[str, ...],
) -> None:
    await junit_upload_mod.upload(
        api_url=api_url,
        token=token,
        repository=repository,
        head_sha=head_sha,
        job_name=job_name,
        provider=provider,
        files=files,
    )
