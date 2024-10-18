import typing


class GitRef(typing.TypedDict):
    ref: str


class HeadRef(typing.TypedDict):
    sha: str


class PullRequest(typing.TypedDict):
    html_url: str
    number: str
    title: str
    body: str | None
    head: HeadRef
    state: str
    draft: bool
    node_id: str
    merged_at: str | None
    merge_commit_sha: str | None


class Comment(typing.TypedDict):
    body: str
    url: str
