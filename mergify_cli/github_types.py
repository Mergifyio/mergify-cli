import typing


class PullRequestRef(typing.TypedDict):
    sha: str
    ref: str


class PullRequest(typing.TypedDict):
    html_url: str
    number: str
    title: str
    body: str | None
    base: PullRequestRef
    head: PullRequestRef
    state: str
    draft: bool
    node_id: str
    merged_at: str | None
    merge_commit_sha: str | None


class Comment(typing.TypedDict):
    body: str
    url: str
