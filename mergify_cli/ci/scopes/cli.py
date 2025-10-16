from __future__ import annotations

import json
import os
import pathlib
import typing
import uuid

import click
import pydantic

from mergify_cli import utils
from mergify_cli.ci.scopes import base_detector
from mergify_cli.ci.scopes import changed_files
from mergify_cli.ci.scopes import config
from mergify_cli.ci.scopes import exceptions


if typing.TYPE_CHECKING:
    from collections import abc

GITHUB_ACTIONS_OUTPUT_NAME = "scopes"


def match_scopes(
    files: abc.Iterable[str],
    filters: dict[config.ScopeName, config.FileFilters],
) -> tuple[set[str], dict[str, list[str]]]:
    scopes_hit: set[str] = set()
    per_scope: dict[str, list[str]] = {s: [] for s in filters}
    for f in files:
        # NOTE(sileht): we use pathlib.full_match to support **, as fnmatch does not
        p = pathlib.PurePosixPath(f)
        for scope, scope_config in filters.items():
            if not scope_config.include and not scope_config.exclude:
                continue

            # Check if file matches any include
            if scope_config.include:
                matches_positive = any(
                    p.full_match(pat) for pat in scope_config.include
                )
            else:
                matches_positive = True

            # Check if file matches any exclude
            matches_negative = any(p.full_match(pat) for pat in scope_config.exclude)

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
            f"{GITHUB_ACTIONS_OUTPUT_NAME}<<{delimiter}\n{json.dumps(data)}\n{delimiter}\n",
        )


def maybe_write_github_step_summary(
    all_scopes: abc.Iterable[str],
    scopes_hit: set[str],
) -> None:
    gha = os.environ.get("GITHUB_STEP_SUMMARY")
    if not gha:
        return
    # Build a pretty Markdown table with emojis
    markdown = "## Mergify CI Scope Matching Results\n\n"
    markdown += "| 🔑 Scope | ✅ Match |\n|:--|:--|\n"
    for scope in sorted(all_scopes):
        emoji = "✅" if scope in scopes_hit else "❌"
        markdown += f"| `{scope}` | {emoji} |\n"

    with pathlib.Path(gha).open("a", encoding="utf-8") as fh:
        fh.write(markdown)


class InvalidDetectedScopeError(exceptions.ScopesError):
    pass


class DetectedScope(pydantic.BaseModel):
    base_ref: str
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


def detect(config_path: str) -> DetectedScope:
    cfg = config.Config.from_yaml(config_path)
    base = base_detector.detect()

    scopes_hit: set[str]
    per_scope: dict[str, list[str]]

    source = cfg.scopes.source
    if source is None:
        all_scopes = set()
        scopes_hit = set()
        per_scope = {}
    elif isinstance(source, config.SourceFiles):
        changed = changed_files.git_changed_files(base.ref)
        all_scopes = set(source.files.keys())
        scopes_hit, per_scope = match_scopes(changed, source.files)
    elif isinstance(source, config.SourceManual):
        msg = "source `manual` has been set, scopes must be send with `scopes-send` or API"
        raise exceptions.ScopesError(msg)
    else:
        msg = "Unsupported source type"  # type:ignore[unreachable]
        raise RuntimeError(msg)

    if cfg.scopes.merge_queue_scope is not None:
        all_scopes.add(cfg.scopes.merge_queue_scope)
        if base.is_merge_queue:
            scopes_hit.add(cfg.scopes.merge_queue_scope)

    click.echo(f"Base: {base.ref}")
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
    maybe_write_github_step_summary(all_scopes, scopes_hit)
    return DetectedScope(base_ref=base.ref, scopes=scopes_hit)


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
