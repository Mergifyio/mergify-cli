import click

from mergify_cli import utils
from mergify_cli.ci import detector
from mergify_cli.ci import upload


ci = click.Group(
    "ci",
    help="Mergify's CI related commands",
)


@ci.command(help="Upload JUnit XML reports")
@click.option(
    "--api-url",
    "-u",
    help="URL of the Mergify API",
    required=True,
    envvar="MERGIFY_API_URL",
    default="https://api.mergify.com",
    show_default=True,
)
@click.option(
    "--token",
    "-t",
    help="CI Issues Application Key",
    required=True,
    envvar="MERGIFY_TOKEN",
)
@click.option(
    "--repository",
    "-r",
    help="Repository full name (owner/repo)",
    required=True,
    default=detector.get_github_repository,
)
@click.option(
    "--test-framework",
    help="Test framework",
)
@click.option(
    "--test-language",
    help="Test language",
)
@click.argument(
    "files",
    nargs=-1,
    required=True,
    type=click.Path(exists=True, dir_okay=False),
)
@utils.run_with_asyncio
async def junit_upload(  # noqa: PLR0913, PLR0917
    api_url: str,
    token: str,
    repository: str,
    test_framework: str | None,
    test_language: str | None,
    files: tuple[str, ...],
) -> None:
    await upload.upload(
        api_url=api_url,
        token=token,
        repository=repository,
        test_framework=test_framework,
        test_language=test_language,
        files=files,
    )
