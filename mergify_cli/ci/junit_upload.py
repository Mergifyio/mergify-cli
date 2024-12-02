from collections import abc
import contextlib
import pathlib
import typing

import httpx

from mergify_cli import console
from mergify_cli import utils


@contextlib.contextmanager
def get_files_to_upload(
    files: tuple[str, ...],
) -> abc.Generator[list[tuple[str, tuple[str, typing.BinaryIO, str]]], None, None]:
    files_to_upload: list[tuple[str, tuple[str, typing.BinaryIO, str]]] = []

    for file in set(files):
        file_path = pathlib.Path(file)
        files_to_upload.append(
            ("files", (file_path.name, file_path.open("rb"), "application/xml")),
        )

    try:
        yield files_to_upload
    finally:
        for _, (_, opened_file, _) in files_to_upload:
            opened_file.close()


async def raise_for_status(response: httpx.Response) -> None:
    if response.is_error:
        await response.aread()
        details = response.text or "<empty_response>"
        console.log(f"[red]Error details: {details}[/]")

    response.raise_for_status()


def get_ci_issues_client(
    api_url: str,
    token: str,
) -> httpx.AsyncClient:
    return utils.get_http_client(
        api_url,
        headers={
            "Authorization": f"Bearer {token}",
        },
        event_hooks={
            "request": [],
            "response": [raise_for_status],
        },
    )


async def upload(  # noqa: PLR0913, PLR0917
    api_url: str,
    token: str,
    repository: str,
    head_sha: str,
    job_name: str,
    provider: str | None,
    files: tuple[str, ...],
) -> None:
    form_data = {
        "head_sha": head_sha,
        "name": job_name,
    }
    if provider is not None:
        form_data["provider"] = provider

    async with get_ci_issues_client(api_url, token) as client:
        with get_files_to_upload(files) as files_to_upload:
            response = await client.post(
                f"/v1/repos/{repository}/ci_issues_upload",
                data=form_data,
                files=files_to_upload,
            )

    gigid = response.json()["gigid"]
    console.print(
        f"::notice title=CI Issues report::CI_ISSUE_GIGID={gigid}",
        soft_wrap=True,
    )
    console.log("[green]:tada: File(s) uploaded[/]")
