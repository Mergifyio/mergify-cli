from __future__ import annotations

import typing

from mergify_cli.stack import setup


if typing.TYPE_CHECKING:
    import pathlib

    import pytest

    from mergify_cli.tests import utils as test_utils


async def test_setup(
    git_mock: test_utils.GitMock,
    tmp_path: pytest.TempdirFactory,
) -> None:
    hooks_dir = typing.cast("pathlib.Path", tmp_path) / ".git" / "hooks"
    hooks_dir.mkdir(parents=True)
    git_mock.mock("rev-parse", "--git-path", "hooks", output=str(hooks_dir))
    await setup.stack_setup()

    hook = hooks_dir / "commit-msg"
    assert hook.exists()
