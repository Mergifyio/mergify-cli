from __future__ import annotations

import pydantic


class GitRef(pydantic.BaseModel):
    model_config = pydantic.ConfigDict(extra="ignore")

    sha: str
    ref: str | None = None


class PullRequest(pydantic.BaseModel):
    model_config = pydantic.ConfigDict(extra="ignore")

    number: int
    title: str | None = None
    body: str | None = None
    base: GitRef | None = None
    head: GitRef | None = None


class Repository(pydantic.BaseModel):
    model_config = pydantic.ConfigDict(extra="ignore")

    default_branch: str | None = None


class GitHubEvent(pydantic.BaseModel):
    model_config = pydantic.ConfigDict(extra="ignore")

    pull_request: PullRequest | None = None
    repository: Repository | None = None
    # push events
    before: str | None = None
    after: str | None = None
