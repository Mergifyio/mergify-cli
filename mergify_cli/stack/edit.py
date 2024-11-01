import os

from mergify_cli import utils


async def stack_edit() -> None:
    os.chdir(await utils.git("rev-parse", "--show-toplevel"))
    trunk = await utils.get_trunk()
    base = await utils.git("merge-base", trunk, "HEAD")
    os.execvp("git", ("git", "rebase", "-i", f"{base}^"))  # noqa: S606
