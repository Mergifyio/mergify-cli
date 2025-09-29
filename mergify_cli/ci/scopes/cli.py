from __future__ import annotations

import json
import os
import pathlib
import subprocess
import typing

import click
import pydantic
import yaml


if typing.TYPE_CHECKING:
    from collections import abc


SCOPE_PREFIX = "scope_"
SCOPE_NAME_RE = r"^[A-Za-z0-9_-]+$"


class ConfigInvalidError(Exception):
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


def _run(cmd: list[str]) -> str:
    try:
        return subprocess.check_output(cmd, text=True, encoding="utf-8").strip()
    except subprocess.CalledProcessError as e:
        msg = f"Command failed: {' '.join(cmd)}\n{e}"
        raise click.ClickException(msg) from e


class MergeQueuePullRequest(typing.TypedDict):
    number: int


class MergeQueueBatchFailed(typing.TypedDict):
    draft_pr_number: int
    checked_pull_request: list[int]


class MergeQueueMetadata(typing.TypedDict):
    checking_base_sha: str
    pull_requests: list[MergeQueuePullRequest]
    previous_failed_batches: list[MergeQueueBatchFailed]


def _yaml_docs_from_fenced_blocks(body: str) -> MergeQueueMetadata | None:
    lines = []
    found = False
    for line in body.splitlines():
        if line.startswith("```yaml"):
            found = True
        elif found:
            if line.startswith("```"):
                break
            lines.append(line)
    if lines:
        return typing.cast("MergeQueueMetadata", yaml.safe_load("\n".join(lines)))
    return None


def _detect_base_from_merge_queue_payload(ev: dict[str, typing.Any]) -> str | None:
    pr = ev.get("pull_request")
    if not isinstance(pr, dict):
        return None
    title = pr.get("title") or ""
    if not isinstance(title, str):
        return None
    if not title.startswith("merge-queue: "):
        return None
    body = pr.get("body") or ""
    content = _yaml_docs_from_fenced_blocks(body)
    if content:
        return content["checking_base_sha"]
    return None


def _detect_base_from_event(ev: dict[str, typing.Any]) -> str | None:
    pr = ev.get("pull_request")
    if isinstance(pr, dict):
        sha = pr.get("base", {}).get("sha")
        if isinstance(sha, str) and sha:
            return sha
    return None


def detect_base() -> str:
    event_path = os.environ.get("GITHUB_EVENT_PATH")
    event: dict[str, typing.Any] | None = None
    if event_path and pathlib.Path(event_path).is_file():
        try:
            with pathlib.Path(event_path).open("r", encoding="utf-8") as f:
                event = json.load(f)
        except FileNotFoundError:
            event = None

    if event is not None:
        # 0) merge-queue PR override
        mq_sha = _detect_base_from_merge_queue_payload(event)
        if mq_sha:
            return mq_sha

        # 1) standard event payload
        event_sha = _detect_base_from_event(event)
        if event_sha:
            return event_sha

    # 2) base ref (e.g., PR target branch)
    base_ref = os.environ.get("GITHUB_BASE_REF")
    if base_ref:
        return base_ref

    msg = (
        "Could not detect base SHA. Ensure checkout has sufficient history "
        "(e.g., actions/checkout with fetch-depth: 0) or provide GITHUB_EVENT_PATH / GITHUB_BASE_REF."
    )
    raise click.ClickException(
        msg,
    )


def git_changed_files(base: str) -> list[str]:
    # Committed changes only between base_sha and HEAD.
    # Includes: Added (A), Copied (C), Modified (M), Renamed (R), Type-changed (T), Deleted (D)
    # Excludes: Unmerged (U), Unknown (X), Broken (B)
    out = _run(
        ["git", "diff", "--name-only", "--diff-filter=ACMRTD", f"{base}...HEAD"],
    )
    return [line for line in out.splitlines() if line]


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
    base = detect_base()
    changed = git_changed_files(base)
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
