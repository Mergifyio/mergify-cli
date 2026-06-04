from __future__ import annotations

import functools
import typing

import pydantic

from mergify_engine import settings
from mergify_engine.config import feature_flag
from mergify_engine.rules import filter as filter_mod


if typing.TYPE_CHECKING:
    from collections import abc
    import re

    from mergify_engine.github import types as github_types


SCOPE_NAME_RE = r"^[A-Za-z0-9_-]+$"


ScopeName = typing.Annotated[
    str,
    pydantic.StringConstraints(pattern=SCOPE_NAME_RE, min_length=2),
]


@functools.lru_cache(maxsize=512)
def _compile_glob_patterns(
    patterns: tuple[str, ...],
) -> tuple[re.Pattern[str], ...]:
    # Cached: `glob.translate` is not cheap and the same scope glob set is
    # matched on every queue add and every `scope` condition evaluation.
    return tuple(filter_mod.glob_pattern_to_regex(p) for p in patterns)


class FileFilters(pydantic.BaseModel):
    # NOTE(sileht): we must be explicit as mergify-cli parent object use
    # extra=ignore
    model_config = pydantic.ConfigDict(extra="forbid")

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

    def matches_any(self, filenames: abc.Iterable[str]) -> bool:
        """Whether at least one of `filenames` belongs to this scope.

        A file belongs when it matches an `include` pattern and no
        `exclude` pattern; `exclude` takes precedence. An empty `include`
        means "everything", so `exclude`-only filtering works.
        """
        includes = _compile_glob_patterns(self.include)
        excludes = _compile_glob_patterns(self.exclude)
        return any(
            (not includes or any(rx.match(filename) for rx in includes))
            and not any(rx.match(filename) for rx in excludes)
            for filename in filenames
        )


class SourceFiles(pydantic.BaseModel):
    # NOTE(sileht): we must be explicit as mergify-cli parent object use
    # extra=ignore
    model_config = pydantic.ConfigDict(extra="forbid")

    files: dict[ScopeName, FileFilters] = pydantic.Field(
        description=(
            "Mapping of scope name to its file filters. "
            "A file belongs to a scope if it matches the scope's `include` "
            "patterns and not its `exclude` patterns."
        ),
    )

    def match_scopes(self, changed_files: abc.Iterable[str]) -> set[str]:
        """Derive the scope names a pull request belongs to from its changed
        files, by matching them against each scope's file filters.
        """
        filenames = list(changed_files)
        return {
            scope_name
            for scope_name, filters in self.files.items()
            if filters.matches_any(filenames)
        }


class SourceManual(pydantic.BaseModel):
    # NOTE(sileht): we must be explicit as mergify-cli parent object use
    # extra=ignore
    model_config = pydantic.ConfigDict(extra="forbid")

    manual: None = pydantic.Field(
        description="Scopes are manually sent via API or `mergify scopes-send`",
    )


class Scopes(pydantic.BaseModel):
    # NOTE(sileht): we must be explicit as mergify-cli parent object use
    # extra=ignore
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
    merge_queue_scope: ScopeName = pydantic.Field(
        default="merge-queue",
        description="Scope name automatically applied to merge queue PRs.",
    )
    capacities: dict[ScopeName, typing.Annotated[int, pydantic.Field(ge=1, le=128)]] = (
        pydantic.Field(
            default_factory=dict,
            description=(
                "How many speculative checks each scope may run at the same "
                "time, as a map of scope name to capacity. Works for any "
                "`source` (file globs or the manual scopes API just decide "
                "membership; capacity is declared here). A scope listed here "
                "runs against its own independent budget; a PR in several "
                "capped scopes consumes one slot from each. PRs whose scopes "
                "are all absent from this map share the repo-wide "
                "`merge_queue.max_parallel_checks` pool. Strict branch "
                "protection still forces an effective capacity of 1."
            ),
            # Pydantic emits the `ScopeName` regex under `patternProperties`,
            # which is permissive — JSON Schema still accepts keys that don't
            # match. Mirror the constraint into `propertyNames` so client-side
            # validators reject malformed scope names (e.g. `"!!"`) up front,
            # matching the engine-side `SCOPE_NAME_RE` check.
            json_schema_extra={
                "propertyNames": {
                    "minLength": 2,
                    "pattern": SCOPE_NAME_RE,
                },
            },
        )
    )


def is_engine_evaluation_enabled(
    owner_id: github_types.GitHubAccountIdType,
) -> bool:
    """Whether the `files`-scopes engine-evaluation feature flag is on for
    this organization.

    Gates only the behavior change (using engine-derived scopes). The queue
    runs a shadow comparison regardless, so parity with the CI path is
    visible before the flag is flipped.
    """
    return feature_flag.is_organization_id_enabled(
        owner_id,
        settings.SCOPES_FILES_ENGINE_EVALUATION_ENABLED_ORGS,
    )


def is_files_evaluated_by_engine(
    source: SourceFiles | SourceManual | None,
    owner_id: github_types.GitHubAccountIdType,
) -> typing.TypeGuard[SourceFiles]:
    """Whether the engine derives `source: files` scopes itself, from the
    pull request's changed files, instead of reading CI-reported
    `GitHubPullRequestScope` rows.

    A significant behavior change for existing `files` users, ramped per
    organization by the feature flag.
    """
    return isinstance(source, SourceFiles) and is_engine_evaluation_enabled(owner_id)


def requires_reported_scopes(
    scopes: Scopes,
    owner_id: github_types.GitHubAccountIdType,
) -> bool:
    """Whether a pull request must wait for scopes to be reported by an
    external actor (the CI action or the scopes API) before it can embark.

    `manual` always waits. `files` waits unless the organization is flagged
    into engine-side evaluation, where the engine derives scopes locally and
    there is nothing to wait for. `None` means no scoping at all.
    """
    return scopes.source is not None and not is_files_evaluated_by_engine(
        scopes.source,
        owner_id,
    )
