#
#  Copyright © 2021-2026 Mergify SAS
#
# Licensed under the Apache License, Version 2.0 (the "License"); you may
# not use this file except in compliance with the License. You may obtain
# a copy of the License at
#
#      http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS, WITHOUT
# WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied. See the
# License for the specific language governing permissions and limitations
# under the License.

from __future__ import annotations

import asyncio
import dataclasses
import json
import re

from mergify_cli import console
from mergify_cli import utils


DEPENDS_ON_RE = re.compile(r"Depends-On:\s*#(\d+)")

# Limit concurrent gh pr view calls for CI checks
_CI_CHECK_SEMAPHORE = asyncio.Semaphore(5)


@dataclasses.dataclass
class DashboardPR:
    number: int
    title: str
    url: str
    repo: str
    is_draft: bool
    body: str
    labels: list[str]

    def to_dict(self) -> dict[str, object]:
        return {
            "number": self.number,
            "title": self.title,
            "url": self.url,
            "repo": self.repo,
            "is_draft": self.is_draft,
            "labels": self.labels,
        }


@dataclasses.dataclass
class DashboardSection:
    title: str
    stacks_by_repo: dict[str, list[list[DashboardPR]]]
    hidden_count: int = 0

    def to_dict(self) -> dict[str, object]:
        return {
            "title": self.title,
            "hidden_count": self.hidden_count,
            "repos": {
                repo: [[pr.to_dict() for pr in stack] for stack in stacks]
                for repo, stacks in self.stacks_by_repo.items()
            },
        }


@dataclasses.dataclass
class DashboardOutput:
    author: str
    org: str
    sections: list[DashboardSection]

    def to_dict(self) -> dict[str, object]:
        return {
            "author": self.author,
            "org": self.org,
            "sections": [s.to_dict() for s in self.sections],
        }


async def _gh_search_prs(query: str) -> list[DashboardPR]:
    """Search PRs using gh CLI and return parsed results."""
    try:
        raw = await utils.run_command(
            "gh",
            "search",
            "prs",
            "--json",
            "number,title,url,repository,isDraft,body,labels",
            "--limit",
            "200",
            query,
        )
    except utils.CommandError:
        return []

    if not raw.strip():
        return []

    results: list[dict[str, object]] = json.loads(raw)
    return [
        DashboardPR(
            number=r["number"],  # type: ignore[arg-type]
            title=r["title"],  # type: ignore[arg-type]
            url=r["url"],  # type: ignore[arg-type]
            repo=r["repository"]["nameWithOwner"],  # type: ignore[index]
            is_draft=r["isDraft"],  # type: ignore[arg-type]
            body=r.get("body") or "",  # type: ignore[arg-type]
            labels=[label["name"] for label in r.get("labels", [])],  # type: ignore[union-attr]
        )
        for r in results
    ]


async def _gh_get_current_user() -> str:
    """Get current GitHub username via gh CLI."""
    return (await utils.run_command("gh", "api", "user", "--jq", ".login")).strip()


async def _gh_pr_ci_passed(repo: str, pr_number: int) -> bool:
    """Check if all CI checks passed for a PR."""
    async with _CI_CHECK_SEMAPHORE:
        try:
            raw = await utils.run_command(
                "gh",
                "pr",
                "view",
                str(pr_number),
                "--repo",
                repo,
                "--json",
                "statusCheckRollup",
            )
        except utils.CommandError:
            return False

    data = json.loads(raw)
    checks: list[dict[str, str]] = data.get("statusCheckRollup", [])
    if not checks:
        return False

    return all(
        c.get("conclusion") == "SUCCESS" or c.get("state") == "SUCCESS"
        for c in checks
        # Skip pending/in-progress checks — only look at completed ones
        if c.get("status", "COMPLETED") == "COMPLETED"
    )


def _parse_depends_on(body: str) -> int | None:
    """Extract the first Depends-On PR number from a PR body."""
    match = DEPENDS_ON_RE.search(body)
    return int(match.group(1)) if match else None


def _build_stacks(prs: list[DashboardPR]) -> list[list[DashboardPR]]:
    """Group PRs into stacks using Depends-On relationships.

    Returns a list of stacks, each ordered bottom-to-top.
    Standalone PRs are returned as single-element lists.
    """
    if not prs:
        return []

    by_number: dict[int, DashboardPR] = {pr.number: pr for pr in prs}

    # Build parent map: child -> parent (the PR it depends on)
    parent_of: dict[int, int] = {}
    for pr in prs:
        dep = _parse_depends_on(pr.body)
        if dep is not None and dep in by_number:
            parent_of[pr.number] = dep

    # Find connected components via union-find
    root_of: dict[int, int] = {}

    def find_root(n: int) -> int:
        while root_of.get(n, n) != n:
            root_of[n] = root_of.get(root_of[n], root_of[n])
            n = root_of[n]
        return n

    def union(a: int, b: int) -> None:
        ra, rb = find_root(a), find_root(b)
        if ra != rb:
            root_of[ra] = rb

    root_of.update({num: num for num in by_number})

    for child, parent in parent_of.items():
        union(child, parent)

    # Group by root
    components: dict[int, list[int]] = {}
    for num in by_number:
        root = find_root(num)
        components.setdefault(root, []).append(num)

    # Order each component bottom-to-top using the dependency chain
    stacks: list[list[DashboardPR]] = []
    for members in components.values():
        member_set = set(members)
        # Children map: parent -> child
        children_of: dict[int, int] = {}
        for num in members:
            if num in parent_of and parent_of[num] in member_set:
                children_of[parent_of[num]] = num

        # Find the bottom: the member that is not a child of anyone in the set
        bottoms = [m for m in members if m not in parent_of or parent_of[m] not in member_set]

        # Walk from bottom to top
        ordered: list[DashboardPR] = []
        if bottoms:
            current = bottoms[0]
            visited: set[int] = set()
            while current in member_set and current not in visited:
                visited.add(current)
                ordered.append(by_number[current])
                current = children_of.get(current, -1)

        # Add any remaining members that weren't reached (shouldn't happen normally)
        reached = {pr.number for pr in ordered}
        ordered.extend(by_number[num] for num in members if num not in reached)

        stacks.append(ordered)

    return stacks


def _group_by_repo(prs: list[DashboardPR]) -> dict[str, list[list[DashboardPR]]]:
    """Group PRs by repo, then build stacks within each repo."""
    by_repo: dict[str, list[DashboardPR]] = {}
    for pr in prs:
        by_repo.setdefault(pr.repo, []).append(pr)

    result: dict[str, list[list[DashboardPR]]] = {}
    for repo in sorted(by_repo):
        result[repo] = _build_stacks(by_repo[repo])
    return result


async def _filter_awaiting_review(
    prs: list[DashboardPR],
    exclude_labels: list[str],
) -> tuple[list[DashboardPR], int]:
    """Filter 'awaiting my review' PRs: exclude by labels and failing CI.

    Returns (visible_prs, hidden_count).
    """
    exclude_labels_lower = {label.lower() for label in exclude_labels}

    # First pass: filter by labels
    label_ok: list[DashboardPR] = []
    hidden = 0
    for pr in prs:
        pr_labels_lower = {label.lower() for label in pr.labels}
        if pr_labels_lower & exclude_labels_lower:
            hidden += 1
        else:
            label_ok.append(pr)

    # Second pass: check CI concurrently for remaining PRs
    async def check_pr(pr: DashboardPR) -> tuple[DashboardPR, bool]:
        passed = await _gh_pr_ci_passed(pr.repo, pr.number)
        return pr, passed

    results = await asyncio.gather(*[check_pr(pr) for pr in label_ok])

    visible: list[DashboardPR] = []
    for pr, ci_passed in results:
        if ci_passed:
            visible.append(pr)
        else:
            hidden += 1

    return visible, hidden


def _display_section(section: DashboardSection) -> None:
    """Render one dashboard section using rich console."""
    console.print(f"\n[bold]{section.title}[/]")
    console.print("─" * len(section.title))

    if not section.stacks_by_repo:
        console.print("  [dim](none)[/]")
        return

    for repo, stacks in section.stacks_by_repo.items():
        console.print(f"  [bold cyan]{repo}[/]")

        for stack in stacks:
            if len(stack) == 1:
                pr = stack[0]
                console.print(
                    f"    #{pr.number} {pr.title}  [link={pr.url}]{pr.url}[/link]",
                )
            else:
                for i, pr in enumerate(stack):
                    is_last = i == len(stack) - 1
                    connector = "└──" if is_last else "├──"
                    console.print(
                        f"    {connector} #{pr.number} {pr.title}  "
                        f"[link={pr.url}]{pr.url}[/link]",
                    )

    if section.hidden_count > 0:
        console.print(
            f"\n  [dim]({section.hidden_count} PRs hidden: excluded labels or failing CI)[/]",
        )


async def get_default_exclude_labels() -> tuple[str, ...]:
    """Get default exclude labels from git config."""
    try:
        result = await utils.git(
            "config",
            "--get",
            "mergify-cli.dashboard-exclude-labels",
        )
        return tuple(part.strip() for part in result.split(",") if part.strip())
    except utils.CommandError:
        return ()


async def get_default_dashboard_org() -> str | None:
    """Get default org from git config."""
    try:
        result = await utils.git("config", "--get", "mergify-cli.dashboard-org")
    except utils.CommandError:
        result = ""
    return result or None


async def stack_dashboard(
    *,
    org: str | None,
    author: str | None = None,
    exclude_labels: list[str],
    output_json: bool = False,
) -> None:
    """Display a dashboard of stacked PRs across the org."""
    if org is None:
        console.print(
            "error: no organization specified. "
            "Use --org or set it via `git config mergify-cli.dashboard-org <org>`.",
            style="red",
        )
        return

    if author is None:
        try:
            author = await _gh_get_current_user()
        except utils.CommandError:
            console.print(
                "error: cannot determine GitHub user. "
                "Use --author or authenticate with `gh auth login`.",
                style="red",
            )
            return

    if not output_json:
        console.print(f"\n[bold]Stack Dashboard[/] [dim]({author} @ {org})[/]")

    # Fetch all three sections concurrently
    wip_prs, team_review_prs, my_review_prs = await asyncio.gather(
        _gh_search_prs(f"is:open is:pr author:{author} org:{org} draft:true"),
        _gh_search_prs(f"is:open is:pr author:{author} org:{org} draft:false"),
        _gh_search_prs(f"is:open is:pr review-requested:{author} draft:false"),
    )

    # Filter "awaiting my review" section
    my_review_visible, hidden_count = await _filter_awaiting_review(
        my_review_prs,
        exclude_labels,
    )

    sections = [
        DashboardSection(
            title="Work in Progress",
            stacks_by_repo=_group_by_repo(wip_prs),
        ),
        DashboardSection(
            title="Awaiting Team Review",
            stacks_by_repo=_group_by_repo(team_review_prs),
        ),
        DashboardSection(
            title="Awaiting My Review",
            stacks_by_repo=_group_by_repo(my_review_visible),
            hidden_count=hidden_count,
        ),
    ]

    output = DashboardOutput(author=author, org=org, sections=sections)

    if output_json:
        console.print(json.dumps(output.to_dict(), indent=2))
    else:
        for section in sections:
            _display_section(section)
        console.print()
