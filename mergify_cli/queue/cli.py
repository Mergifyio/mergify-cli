from __future__ import annotations

import asyncio
import datetime

import click
from rich.text import Text
from rich.tree import Tree

from mergify_cli import console
from mergify_cli import utils
from mergify_cli.dym import DYMGroup
from mergify_cli.queue import api as queue_api


STATUS_STYLES: dict[str, tuple[str, str]] = {
    "running": ("●", "green"),
    "bisecting": ("◑", "yellow"),
    "preparing": ("◌", "blue"),
    "failed": ("✗", "red"),
    "merged": ("✓", "dim green"),
    "waiting_for_merge": ("◎", "cyan"),
    "waiting_for_previous_batches": ("⏳", "yellow"),
    "waiting_for_requeue": ("↻", "yellow"),
    "waiting_schedule": ("⏰", "yellow"),
    "waiting_for_batch": ("⏳", "dim"),
    "frozen": ("❄", "cyan"),
}

CHECK_STATE_STYLES: dict[str, tuple[str, str]] = {
    "success": ("✓", "green"),
    "pending": ("◌", "blue"),
    "failure": ("✗", "red"),
    "error": ("✗", "red"),
    "cancelled": ("○", "dim"),
    "action_required": ("!", "yellow"),
    "timed_out": ("⏰", "red"),
    "neutral": ("○", "dim"),
    "skipped": ("○", "dim"),
    "stale": ("○", "dim"),
}


def _relative_time(iso_str: str | None, *, future: bool = False) -> str:
    if not iso_str:
        return ""
    try:
        dt = datetime.datetime.fromisoformat(iso_str)
    except ValueError:
        return iso_str
    now = datetime.datetime.now(tz=datetime.UTC)
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=datetime.UTC)
    delta = abs(now - dt)
    total_seconds = int(delta.total_seconds())
    if total_seconds < 60:
        value = f"{total_seconds}s"
    elif total_seconds < 3600:
        value = f"{total_seconds // 60}m"
    elif total_seconds < 86400:
        value = f"{total_seconds // 3600}h"
    else:
        value = f"{total_seconds // 86400}d"
    if future:
        return f"~{value}"
    return f"{value} ago"


def _status_text(code: str) -> Text:
    icon, style = STATUS_STYLES.get(code, ("?", "dim"))
    text = Text()
    text.append(f"{icon} ", style=style)
    text.append(code, style=style)
    return text


def _batch_label(batch: queue_api.QueueBatch) -> Text:
    label = _status_text(batch["status"]["code"])
    checks = batch["checks_summary"]
    if checks["total"] > 0:
        label.append(f"   checks {checks['passed']}/{checks['total']}", style="dim")
    started = batch.get("started_at")
    if started:
        rel = _relative_time(started)
        if rel:
            label.append(f"   {rel}", style="dim")
    eta = batch.get("estimated_merge_at")
    if eta:
        rel = _relative_time(eta, future=True)
        if rel:
            label.append(f"   ETA {rel}", style="dim")
    return label


def _pr_label(pr: queue_api.QueuePullRequest) -> Text:
    text = Text()
    text.append(f"#{pr['number']}", style="cyan")
    text.append(f" {pr['title']}")
    text.append(f" ({pr['author']['login']})", style="dim")
    return text


def _topological_sort(
    batches: list[queue_api.QueueBatch],
) -> list[queue_api.QueueBatch]:
    id_to_batch = {b["id"]: b for b in batches}
    visited: set[str] = set()
    result: list[queue_api.QueueBatch] = []

    def visit(batch_id: str) -> None:
        if batch_id in visited:
            return
        visited.add(batch_id)
        batch = id_to_batch.get(batch_id)
        if batch is None:
            return
        for parent_id in batch.get("parent_ids") or []:
            visit(parent_id)
        result.append(batch)

    for b in batches:
        visit(b["id"])
    return result


def _group_batches_by_scope(
    batches: list[queue_api.QueueBatch],
) -> dict[str, list[queue_api.QueueBatch]]:
    groups: dict[str, list[queue_api.QueueBatch]] = {}
    for batch in batches:
        scopes = batch.get("scopes") or ["default"]
        for scope in scopes:
            groups.setdefault(scope, []).append(batch)
    return groups


def _print_batches(batches: list[queue_api.QueueBatch]) -> None:
    sorted_batches = _topological_sort(batches)
    scope_groups = _group_batches_by_scope(sorted_batches)
    all_scopes = list(scope_groups.keys())
    single_scope = len(all_scopes) == 1

    for scope in all_scopes:
        scope_batches = scope_groups[scope]
        label = "Batches" if single_scope else scope
        tree = Tree(Text(label, style="bold"))
        for batch in scope_batches:
            batch_node = tree.add(_batch_label(batch))
            for pr in batch["pull_requests"]:
                batch_node.add(_pr_label(pr))
        console.print(tree)


def _print_waiting_prs(pull_requests: list[queue_api.QueuePullRequest]) -> None:
    console.print(Text("Waiting", style="bold"))
    for pr in pull_requests:
        line = Text("  ")
        line.append(f"#{pr['number']}", style="cyan")
        line.append(f"  {pr['title']}")
        line.append(f"  {pr['author']['login']}", style="dim")
        line.append(f"  {pr['priority_alias']}", style="magenta")
        queued_rel = _relative_time(pr["queued_at"])
        if queued_rel:
            line.append(f"  queued {queued_rel}", style="dim")
        eta = pr.get("estimated_merge_at")
        if eta:
            eta_rel = _relative_time(eta, future=True)
            if eta_rel:
                line.append(f"  ETA {eta_rel}", style="dim")
        console.print(line)


def _print_pull_metadata(data: queue_api.QueuePullResponse) -> None:
    console.print(Text(f"PR #{data['number']}", style="bold"))
    console.print()
    console.print(f"  Position:    {data['position']}")
    console.print(f"  Priority:    {data['priority_rule_name']}")
    console.print(f"  Queue rule:  {data['queue_rule_name']}")
    queued_rel = _relative_time(data["queued_at"])
    console.print(
        f"  Queued at:   {queued_rel}"
        if queued_rel
        else f"  Queued at:   {data['queued_at']}",
    )
    eta = data.get("estimated_time_of_merge")
    eta_rel = _relative_time(eta, future=True) if eta else ""
    console.print(f"  ETA:         {eta_rel}" if eta_rel else "  ETA:         -")


def _check_state_text(state: str) -> Text:
    icon, style = CHECK_STATE_STYLES.get(state, ("?", "dim"))
    text = Text()
    text.append(f"{icon} ", style=style)
    text.append(state, style=style)
    return text


def _print_checks_section(mc: queue_api.QueueMergeabilityCheck) -> None:
    from rich.table import Table

    console.print()
    ci_label = Text("  CI State: ")
    ci_label.append_text(_check_state_text(mc["ci_state"]))
    ci_label.append(f"   {mc['check_type']}", style="dim")
    started = mc.get("started_at")
    if started:
        rel = _relative_time(started)
        if rel:
            ci_label.append(f"   started {rel}", style="dim")
    console.print(ci_label)

    checks = mc["checks"]
    if checks:
        table = Table(show_header=True, padding=(0, 1), box=None)
        table.add_column("  Check", style="dim")
        table.add_column("Status")
        for check in checks:
            table.add_row(f"  {check['name']}", _check_state_text(check["state"]))
        console.print(table)


def _print_conditions_tree(
    evaluation: queue_api.QueueConditionEvaluation,
) -> None:
    console.print()
    tree = Tree(Text("Conditions", style="bold"))
    _add_condition_nodes(tree, evaluation)
    console.print(tree)


def _add_condition_nodes(
    parent: Tree,
    evaluation: queue_api.QueueConditionEvaluation,
) -> None:
    subconditions = evaluation.get("subconditions") or []
    if subconditions:
        for sub in subconditions:
            icon = "✓" if sub["match"] else "✗"
            style = "green" if sub["match"] else "red"
            label = Text()
            label.append(f"{icon} ", style=style)
            label.append(sub["label"])
            node = parent.add(label)
            _add_condition_nodes(node, sub)


@click.group(
    cls=DYMGroup,
    invoke_without_command=True,
    help="Manage merge queue",
)
@click.option(
    "--token",
    "-t",
    help="Mergify or GitHub token",
    envvar=["MERGIFY_TOKEN", "GITHUB_TOKEN"],
    required=True,
    default=lambda: asyncio.run(utils.get_default_token()),
)
@click.option(
    "--api-url",
    "-u",
    help="URL of the Mergify API",
    envvar="MERGIFY_API_URL",
    default="https://api.mergify.com",
    show_default=True,
)
@click.option(
    "--repository",
    "-r",
    help="Repository full name (owner/repo)",
    required=True,
    default=lambda: asyncio.run(utils.get_default_repository()),
)
@click.pass_context
def queue(
    ctx: click.Context,
    *,
    token: str,
    api_url: str,
    repository: str,
) -> None:
    ctx.ensure_object(dict)
    ctx.obj["token"] = token
    ctx.obj["api_url"] = api_url
    ctx.obj["repository"] = repository
    if ctx.invoked_subcommand is None:
        click.echo(ctx.get_help())


@queue.command(help="Show merge queue status for the repository")
@click.option(
    "--branch",
    "-b",
    default=None,
    help="Branch name to filter the queue",
)
@click.option(
    "--json",
    "output_json",
    is_flag=True,
    help="Output in JSON format",
)
@click.pass_context
@utils.run_with_asyncio
async def status(ctx: click.Context, *, branch: str | None, output_json: bool) -> None:
    async with utils.get_mergify_http_client(
        ctx.obj["api_url"],
        ctx.obj["token"],
    ) as client:
        data = await queue_api.get_queue_status(
            client,
            ctx.obj["repository"],
            branch=branch,
        )

    if output_json:
        import json

        click.echo(json.dumps(data, indent=2))
        return

    console.print(
        Text(f"Merge Queue: {ctx.obj['repository']}", style="bold"),
    )
    console.print()

    pause = data.get("pause")
    if pause is not None:
        pause_rel = _relative_time(pause["paused_at"])
        pause_text = Text()
        pause_text.append("⚠  Queue is paused: ", style="bold yellow")
        pause_text.append(f'"{pause["reason"]}"')
        if pause_rel:
            pause_text.append(f" (since {pause_rel})", style="dim")
        console.print(pause_text)
        console.print()

    batches = data["batches"]
    waiting = data["waiting_pull_requests"]

    if not batches and not waiting:
        console.print("Queue is empty")
        return

    if batches:
        _print_batches(batches)

    if waiting:
        if batches:
            console.print()
        _print_waiting_prs(waiting)


@queue.command(help="Show detailed state of a pull request in the merge queue")
@click.argument("pr_number", type=int)
@click.option(
    "--json",
    "output_json",
    is_flag=True,
    help="Output in JSON format",
)
@click.pass_context
@utils.run_with_asyncio
async def show(ctx: click.Context, *, pr_number: int, output_json: bool) -> None:
    import httpx

    try:
        async with utils.get_mergify_http_client(
            ctx.obj["api_url"],
            ctx.obj["token"],
        ) as client:
            data = await queue_api.get_queue_pull(
                client,
                ctx.obj["repository"],
                pr_number,
            )
    except httpx.HTTPStatusError as e:
        if e.response.status_code == 404:
            console.print(
                f"PR #{pr_number} is not in the merge queue",
                style="yellow",
            )
            raise SystemExit(1) from None
        raise

    if output_json:
        import json

        click.echo(json.dumps(data, indent=2))
        return

    _print_pull_metadata(data)

    mc = data.get("mergeability_check")
    if mc is None:
        console.print()
        console.print("  Waiting for mergeability check...", style="dim")
    else:
        _print_checks_section(mc)
        conditions = mc.get("conditions_evaluation")
        if conditions is not None:
            _print_conditions_tree(conditions)
