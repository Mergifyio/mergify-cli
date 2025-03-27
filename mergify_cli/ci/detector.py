import json
import os
import pathlib
import re
import typing
from urllib import parse

from mergify_cli import utils


CIProviderT = typing.Literal["github_actions", "circleci"]


def get_ci_provider() -> CIProviderT | None:
    if os.getenv("GITHUB_ACTIONS") == "true":
        return "github_actions"
    if os.getenv("CIRCLECI") == "true":
        return "circleci"
    return None


def get_job_name() -> str | None:
    if get_ci_provider() == "github_actions":
        return os.getenv("GITHUB_JOB")
    if get_ci_provider() == "circleci":
        return os.getenv("CIRCLE_JOB")

    return None


def get_head_ref_name() -> str | None:
    match get_ci_provider():
        case "github_actions":
            return os.getenv("GITHUB_REF_NAME")
        case "circleci":
            return os.getenv("CIRCLE_BRANCH")
        case _:
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
    if get_ci_provider() == "github_actions":
        return get_github_actions_head_sha()
    if get_ci_provider() == "circleci":
        return await get_circle_ci_head_sha()

    return None


def get_cicd_pipeline_runner_name() -> str | None:
    if get_ci_provider() == "github_actions" and "RUNNER_NAME" in os.environ:
        return os.environ["RUNNER_NAME"]
    return None


def get_cicd_pipeline_run_id() -> int | None:
    if get_ci_provider() == "github_actions" and "GITHUB_RUN_ID" in os.environ:
        return int(os.environ["GITHUB_RUN_ID"])

    if get_ci_provider() == "circleci" and "CIRCLE_WORKFLOW_ID" in os.environ:
        return int(os.environ["CIRCLE_WORKFLOW_ID"])

    return None


def get_cicd_pipeline_run_attempt() -> int | None:
    if get_ci_provider() == "github_actions" and "GITHUB_RUN_ATTEMPT" in os.environ:
        return int(os.environ["GITHUB_RUN_ATTEMPT"])
    if get_ci_provider() == "circleci" and "CIRCLE_BUILD_NUM" in os.environ:
        return int(os.environ["CIRCLE_BUILD_NUM"])

    return None


def get_github_repository() -> str | None:
    if get_ci_provider() == "github_actions":
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

    return None
