from __future__ import annotations

import os
import pathlib
import typing

import click
import pydantic
import yaml

from mergify_cli.ci.scopes import base_detector
from mergify_cli.ci.scopes import changed_files


if typing.TYPE_CHECKING:
    from collections import abc


SCOPE_PREFIX = "scope_"
SCOPE_NAME_RE = r"^[A-Za-z0-9_-]+$"


class ScopesError(Exception):
    pass


class ConfigInvalidError(ScopesError):
    pass


class ChangedFilesError(ScopesError):
    pass


class ScopeConfig(pydantic.BaseModel):
    include: tuple[str, ...] = pydantic.Field(default_factory=tuple)
    exclude: tuple[str, ...] = pydantic.Field(default_factory=tuple)


ScopeName = typing.Annotated[
    str,
    pydantic.StringConstraints(pattern=SCOPE_NAME_RE, min_length=1),
]


class Config(pydantic.BaseModel):
    scopes: dict[ScopeName, ScopeConfig]

    @classmethod
    def from_dict(cls, data: dict[str, typing.Any] | typing.Any) -> Config:  # noqa: ANN401
        try:
            return cls.model_validate(data)
        except pydantic.ValidationError as e:
            raise ConfigInvalidError(e)

    @classmethod
    def from_yaml(cls, path: str) -> Config:
        with pathlib.Path(path).open(encoding="utf-8") as f:
            try:
                data = yaml.safe_load(f) or {}
            except yaml.YAMLError as e:
                raise ConfigInvalidError(e)

            return cls.from_dict(data)


def match_scopes(
    config: Config,
    files: abc.Iterable[str],
) -> tuple[set[str], dict[str, list[str]]]:
    scopes_hit: set[str] = set()
    per_scope: dict[str, list[str]] = {s: [] for s in config.scopes}
    for f in files:
        # NOTE(sileht): we use pathlib.full_match to support **, as fnmatch does not
        p = pathlib.PurePosixPath(f)
        for scope, scope_config in config.scopes.items():
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
    with pathlib.Path(gha).open("a", encoding="utf-8") as fh:
        for s in sorted(all_scopes):
            key = f"{SCOPE_PREFIX}{s}"
            val = "true" if s in scopes_hit else "false"
            fh.write(f"{key}={val}\n")


def detect(config_path: str) -> None:
    cfg = Config.from_yaml(config_path)
    base = base_detector.detect()
    try:
        changed = changed_files.git_changed_files(base)
    except changed_files.ChangedFilesError as e:
        raise ChangedFilesError(str(e))
    scopes_hit, per_scope = match_scopes(cfg, changed)

    click.echo(f"Base: {base}")
    if scopes_hit:
        click.echo("Scopes touched:")
        for s in sorted(scopes_hit):
            click.echo(f"- {s}")
            if os.environ.get("ACTIONS_STEP_DEBUG") == "true":
                for f in sorted(per_scope.get(s, [])):
                    click.echo(f"    {f}")
    else:
        click.echo("No scopes matched.")

    maybe_write_github_outputs(cfg.scopes.keys(), scopes_hit)
