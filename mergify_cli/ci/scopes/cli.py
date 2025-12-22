from __future__ import annotations

import glob
import json
import os
import pathlib
import re
import typing
import uuid

import click
import pydantic

from mergify_cli import utils
from mergify_cli.ci.scopes import changed_files
from mergify_cli.ci.scopes import config
from mergify_cli.ci.scopes import exceptions


if typing.TYPE_CHECKING:
    from collections import abc

    from mergify_cli.ci.git_refs import detector as git_refs_detector

GITHUB_ACTIONS_SCOPES_OUTPUT_NAME = "scopes"


# NOTE: We convert the pattern to a compiled regex using `glob.translate`,
# in order to avoid any potential inconsistency that could arise from
# running `glob.glob` or pathlib.PurePath.full_match` on a different OS.
def convert_pattern_to_regex(pattern: str) -> re.Pattern[str]:
    return re.compile(
        glob.translate(
            pattern,
            recursive=True,
            include_hidden=True,
            seps=["/", "\\"],
        ),
    )


def match_scopes(
    files: abc.Iterable[str],
    filters: dict[config.ScopeName, config.FileFilters],
) -> tuple[set[str], dict[str, list[str]]]:
    scopes_hit: set[str] = set()
    per_scope: dict[str, list[str]] = {s: [] for s in filters}
    for f in files:
        for scope, scope_config in filters.items():
            if not scope_config.include and not scope_config.exclude:
                continue

            # Check if file matches any include
            if scope_config.include:
                matches_positive = any(
                    convert_pattern_to_regex(pat).fullmatch(f)
                    for pat in scope_config.include
                )
            else:
                matches_positive = True

            # Check if file matches any exclude
            matches_negative = any(
                convert_pattern_to_regex(pat).fullmatch(f)
                for pat in scope_config.exclude
            )

            # File matches the scope if it matches positive patterns and doesn't match negative patterns
            if matches_positive and not matches_negative:
                scopes_hit.add(scope)
                per_scope[scope].append(f)
    return scopes_hit, {k: v for k, v in per_scope.items() if v}


def maybe_write_github_outputs(
    all_scopes: abc.Iterable[str],
    scopes_hit: set[str],
) -> None:
    gha = os.environ.get("GITHUB_OUTPUT")
    if not gha:
        return
    delimiter = f"ghadelimiter_{uuid.uuid4()}"
    with pathlib.Path(gha).open("a", encoding="utf-8") as fh:
        # NOTE(sileht): Boolean in GitHub Workflow should be avoided.
        # In GHA, an output is a string, so putting a bool in the JSON
        # will be a bool when the JSON is parsed, but once copied into another output
        # it's converted to the string "false|true". To avoid any confusion about whether it's
        # a bool or a string, make it always a string.
        data = {
            key: "true" if key in scopes_hit else "false" for key in sorted(all_scopes)
        }
        fh.write(
            f"{GITHUB_ACTIONS_SCOPES_OUTPUT_NAME}<<{delimiter}\n{json.dumps(data)}\n{delimiter}\n",
        )


def maybe_write_github_step_summary(
    references: git_refs_detector.References,
    all_scopes: abc.Iterable[str],
    scopes_hit: set[str],
) -> None:
    gha = os.environ.get("GITHUB_STEP_SUMMARY")
    if not gha:
        return
    # Build a pretty Markdown table with emojis
    markdown = "## Mergify CI Scope Matching Results"
    if references.base is not None:
        markdown += f" for `{references.base[:7]}...{references.head[:7]}` (source: `{references.source}`)"
    markdown += "\n\n"
    markdown += "| 🎯 Scope | ✅ Match |\n|:--|:--|\n"
    for scope in sorted(all_scopes):
        emoji = "✅" if scope in scopes_hit else "❌"
        markdown += f"| `{scope}` | {emoji} |\n"

    with pathlib.Path(gha).open("a", encoding="utf-8") as fh:
        fh.write(markdown)


class InvalidDetectedScopeError(exceptions.ScopesError):
    pass


class DetectedScope(pydantic.BaseModel):
    scopes: set[str]

    def save_to_file(self, file: str) -> None:
        with pathlib.Path(file).open("w", encoding="utf-8") as f:
            f.write(self.model_dump_json())

    @classmethod
    def load_from_file(cls, filename: str) -> DetectedScope:
        with pathlib.Path(filename).open("r", encoding="utf-8") as f:
            try:
                return cls.model_validate_json(f.read())
            except pydantic.ValidationError as e:
                raise InvalidDetectedScopeError(str(e))


def detect(
    config_path: str,
    *,
    references: git_refs_detector.References,
) -> DetectedScope:
    cfg = config.Config.from_yaml(config_path)

    if references.base is not None:
        click.echo(f"Base: {references.base}")
    click.echo(f"Head: {references.head}")
    click.echo(f"Source: {references.source}")

    scopes_hit: set[str]
    per_scope: dict[str, list[str]]

    source = cfg.scopes.source
    if source is None:
        all_scopes = set()
        scopes_hit = set()
        per_scope = {}
    elif isinstance(source, config.SourceFiles):
        all_scopes = set(source.files.keys())
        if references.base is None:
            click.echo("No base provided, selecting all scopes")
            scopes_hit = set(source.files.keys())
            per_scope = {}
        else:
            changed = changed_files.git_changed_files(references.base, references.head)
            click.echo("Changed files detected:")
            for file in changed:
                click.echo(f"- {file}")
            scopes_hit, per_scope = match_scopes(changed, source.files)
    elif isinstance(source, config.SourceManual):
        msg = "source `manual` has been set, scopes must be sent with `scopes-send` or API"
        raise exceptions.ScopesError(msg)
    else:
        msg = "Unsupported source type"  # type:ignore[unreachable]
        raise RuntimeError(msg)

    if cfg.scopes.merge_queue_scope is not None:
        all_scopes.add(cfg.scopes.merge_queue_scope)
        if references.source == "merge_queue":
            scopes_hit.add(cfg.scopes.merge_queue_scope)

    if scopes_hit:
        click.echo("Scopes touched:")
        for s in sorted(scopes_hit):
            click.echo(f"- {s}")
            if os.environ.get("ACTIONS_STEP_DEBUG") == "true":
                for f in sorted(per_scope.get(s, [])):
                    click.echo(f"    {f}")
    else:
        click.echo("No scopes matched.")

    maybe_write_github_outputs(all_scopes, scopes_hit)
    maybe_write_github_step_summary(
        references,
        all_scopes,
        scopes_hit,
    )
    return DetectedScope(scopes=scopes_hit)


async def send_scopes(
    api_url: str,
    token: str,
    repository: str,
    pull_request: int,
    scopes: list[str],
) -> None:
    client = utils.get_mergify_http_client(api_url, token)
    await client.post(
        f"/v1/repos/{repository}/pulls/{pull_request}/scopes",
        json={"scopes": scopes},
    )
