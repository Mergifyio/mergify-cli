from __future__ import annotations

import typing

import pydantic


SCOPE_NAME_RE = r"^[A-Za-z0-9_-]+$"


ScopeName = typing.Annotated[
    str,
    pydantic.StringConstraints(pattern=SCOPE_NAME_RE, min_length=1),
]


class FileFilters(pydantic.BaseModel):
    include: tuple[str, ...] = pydantic.Field(
        default_factory=lambda: ("**/*",),
        description=(
            "Glob patterns of files to include for this scope. "
            "Empty means 'include everything' before exclusions. "
            "Examples: ('src/**/*.py', 'Makefile')"
        ),
    )
    exclude: tuple[str, ...] = pydantic.Field(
        default_factory=tuple,
        description=(
            "Glob patterns of files to exclude from this scope. "
            "Evaluated after `include` and takes precedence. "
            "Examples: ('**/tests/**', '*.md')"
        ),
    )


class SourceFiles(pydantic.BaseModel):
    files: dict[ScopeName, FileFilters] = pydantic.Field(
        description=(
            "Mapping of scope name to its file filters. "
            "A file belongs to a scope if it matches the scope's `include` "
            "patterns and not its `exclude` patterns."
        ),
    )


class SourceManual(pydantic.BaseModel):
    manual: None = pydantic.Field(
        description="Scopes are manually sent via API or `mergify scopes-send`",
    )


class Scopes(pydantic.BaseModel):
    model_config = pydantic.ConfigDict(extra="forbid")

    source: SourceFiles | SourceManual | None = pydantic.Field(
        default=None,
        description=(
            "Where scopes come from. "
            "`files` uses file-pattern rules (`gha-mergify-ci-scopes` must have been setup on your pull request); "
            "`manual` uses scopes sent via API or `mergify scopes-send`; "
            "`None` disables scoping."
        ),
    )
    merge_queue_scope: str | None = pydantic.Field(
        default="merge-queue",
        description=(
            "Optional scope name automatically applied to merge queue PRs. "
            "Set to `None` to disable."
        ),
    )
