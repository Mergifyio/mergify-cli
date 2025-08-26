import json
import os
import pathlib
import re
import typing
from urllib import parse

from mergify_cli import utils


GIT_BRANCH_PREFIXES = ("origin/", "refs/heads/")

CIProviderT = typing.Literal["github_actions", "circleci", "jenkins"]


def get_ci_provider() -> CIProviderT | None:
    if os.getenv("JENKINS_URL"):
        return "jenkins"
    if os.getenv("GITHUB_ACTIONS") == "true":
        return "github_actions"
    if os.getenv("CIRCLECI") == "true":
        return "circleci"
    return None


def get_pipeline_name() -> str | None:
    match get_ci_provider():
        case "github_actions":
            return os.getenv("GITHUB_WORKFLOW")
        case "jenkins":
            return os.getenv("JOB_NAME")

    return None


def get_job_name() -> str | None:
    match get_ci_provider():
        case "github_actions":
            return os.getenv("GITHUB_JOB")
        case "circleci":
            return os.getenv("CIRCLE_JOB")
        case "jenkins":
            return os.getenv("JOB_NAME")
        case _:
            return None


def get_jenkins_head_ref_name() -> str | None:
    branch = os.getenv("GIT_BRANCH")
    if branch:
        # NOTE(sileht): it's not 100% bullet proof but since it's very complicated
        # and unlikely to change/add a remote with Jenkins Git/GitHub plugins,
        # we just handle the most common cases.
        for prefix in GIT_BRANCH_PREFIXES:
            if branch.startswith(prefix):
                return branch[len(prefix) :]
        return branch
    return None


def get_head_ref_name() -> str | None:
    match get_ci_provider():
        case "github_actions":
            return os.getenv("GITHUB_REF_NAME")
        case "circleci":
            return os.getenv("CIRCLE_BRANCH")
        case "jenkins":
            return get_jenkins_head_ref_name()
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
    match get_ci_provider():
        case "github_actions":
            return get_github_actions_head_sha()
        case "circleci":
            return await get_circle_ci_head_sha()
        case "jenkins":
            return os.getenv("GIT_COMMIT")
        case _:
            return None


def get_cicd_pipeline_runner_name() -> str | None:
    match get_ci_provider():
        case "github_actions":
            return os.getenv("RUNNER_NAME")
        case "jenkins":
            return os.getenv("NODE_NAME")
        case _:
            return None


def get_cicd_pipeline_run_id() -> int | str | None:
    match get_ci_provider():
        case "github_actions":
            if "GITHUB_RUN_ID" in os.environ:
                return int(os.environ["GITHUB_RUN_ID"])
        case "circleci":
            if "CIRCLE_WORKFLOW_ID" in os.environ:
                return int(os.environ["CIRCLE_WORKFLOW_ID"])
        case "jenkins":
            return os.getenv("BUILD_ID")

    return None


def get_cicd_pipeline_run_attempt() -> int | None:
    if get_ci_provider() == "github_actions" and "GITHUB_RUN_ATTEMPT" in os.environ:
        return int(os.environ["GITHUB_RUN_ATTEMPT"])
    if get_ci_provider() == "circleci" and "CIRCLE_BUILD_NUM" in os.environ:
        return int(os.environ["CIRCLE_BUILD_NUM"])

    return None


def _get_github_repository_from_env(env: str) -> str | None:
    repository_url = os.getenv(env)
    if repository_url is None:
        return None

    # Handle SSH Git URLs like git@github.com:owner/repo.git
    if match := re.match(
        r"git@[\w.-]+:(?P<full_name>[\w.-]+/[\w.-]+)(?:\.git)?/?$",
        repository_url,
    ):
        full_name = match.group("full_name")
        return full_name.removesuffix(".git")

    # Handle HTTPS/HTTP URLs like https://github.com/owner/repo (with optional port)
    if match := re.match(
        r"(https?://[\w.-]+(?::\d+)?/)?(?P<full_name>[\w.-]+/[\w.-]+)/?$",
        repository_url,
    ):
        return match.group("full_name")

    return None


def get_github_repository() -> str | None:
    match get_ci_provider():
        case "github_actions":
            return os.getenv("GITHUB_REPOSITORY")
        case "circleci":
            return _get_github_repository_from_env("CIRCLE_REPOSITORY_URL")
        case "jenkins":
            return _get_github_repository_from_env("GIT_URL")
        case _:
            return None
