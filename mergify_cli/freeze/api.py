from __future__ import annotations

import typing


if typing.TYPE_CHECKING:
    import datetime
    import uuid

    import httpx


async def list_freezes(
    client: httpx.AsyncClient,
    repository: str,
) -> list[dict[str, typing.Any]]:
    response = await client.get(
        f"/v1/repos/{repository}/scheduled_freeze",
    )
    return response.json()["scheduled_freezes"]  # type: ignore[no-any-return]


async def create_freeze(
    client: httpx.AsyncClient,
    repository: str,
    *,
    reason: str,
    timezone: str,
    matching_conditions: list[str],
    start: datetime.datetime | None = None,
    end: datetime.datetime | None = None,
    exclude_conditions: list[str] | None = None,
) -> dict[str, typing.Any]:
    payload: dict[str, typing.Any] = {
        "reason": reason,
        "timezone": timezone,
        "matching_conditions": matching_conditions,
    }
    if start is not None:
        payload["start"] = start.isoformat()
    if end is not None:
        payload["end"] = end.isoformat()
    if exclude_conditions:
        payload["exclude_conditions"] = exclude_conditions

    response = await client.post(
        f"/v1/repos/{repository}/scheduled_freeze",
        json=payload,
    )
    return response.json()  # type: ignore[no-any-return]


async def update_freeze(
    client: httpx.AsyncClient,
    repository: str,
    freeze_id: uuid.UUID,
    *,
    reason: str,
    timezone: str,
    matching_conditions: list[str],
    start: datetime.datetime | None = None,
    end: datetime.datetime | None = None,
    exclude_conditions: list[str] | None = None,
) -> dict[str, typing.Any]:
    payload: dict[str, typing.Any] = {
        "reason": reason,
        "timezone": timezone,
        "matching_conditions": matching_conditions,
    }
    if start is not None:
        payload["start"] = start.isoformat()
    if end is not None:
        payload["end"] = end.isoformat()
    if exclude_conditions is not None:
        payload["exclude_conditions"] = exclude_conditions

    response = await client.patch(
        f"/v1/repos/{repository}/scheduled_freeze/{freeze_id}",
        json=payload,
    )
    return response.json()  # type: ignore[no-any-return]


async def delete_freeze(
    client: httpx.AsyncClient,
    repository: str,
    freeze_id: uuid.UUID,
    *,
    delete_reason: str | None = None,
) -> None:
    url = f"/v1/repos/{repository}/scheduled_freeze/{freeze_id}"
    if delete_reason is not None:
        await client.request(
            "DELETE",
            url,
            json={"delete_reason": delete_reason},
        )
    else:
        await client.delete(url)
