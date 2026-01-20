from __future__ import annotations

import json
import pathlib
from typing import TYPE_CHECKING

import pytest

from mergify_cli.ci import detector


if TYPE_CHECKING:
    import respx


PULL_REQUEST_EVENT = pathlib.Path(__file__).parent / "pull_request.json"


def test_get_head_branch_jenkins(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("GIT_BRANCH", "origin/main")

    assert detector.get_jenkins_head_ref_name() == "main"


@pytest.mark.parametrize(
    ("head_ref", "ref_name", "expected"),
    [
        pytest.param(
            "feature-branch",
            "123/merge",
            "feature-branch",
            id="GITHUB_HEAD_REF takes precedence",
        ),
        pytest.param(
            None,
            "main",
            "main",
            id="GITHUB_REF_NAME fallback when GITHUB_HEAD_REF is not set",
        ),
        pytest.param(
            "",
            "main",
            "main",
            id="GITHUB_REF_NAME fallback when GITHUB_HEAD_REF is empty",
        ),
        pytest.param(
            None,
            None,
            None,
            id="None when neither GITHUB_HEAD_REF nor GITHUB_REF_NAME is set",
        ),
    ],
)
def test_get_github_actions_head_ref_name(
    monkeypatch: pytest.MonkeyPatch,
    head_ref: str | None,
    ref_name: str | None,
    expected: str | None,
) -> None:
    monkeypatch.delenv("GITHUB_HEAD_REF", raising=False)
    monkeypatch.delenv("GITHUB_REF_NAME", raising=False)

    if head_ref:
        monkeypatch.setenv("GITHUB_HEAD_REF", head_ref)
    if ref_name:
        monkeypatch.setenv("GITHUB_REF_NAME", ref_name)

    result = detector.get_github_actions_head_ref_name()
    assert result == expected


def test_get_head_sha_github_actions(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("GITHUB_ACTIONS", "true")
    monkeypatch.setenv("GITHUB_EVENT_NAME", "pull_request")
    monkeypatch.setenv("GITHUB_EVENT_PATH", str(PULL_REQUEST_EVENT))

    assert (
        detector.get_github_actions_head_sha()
        == "ec26c3e57ca3a959ca5aad62de7213c562f8c821"
    )


@pytest.mark.parametrize(
    ("url", "api_url"),
    [
        ("https://enterprise-ghes.com", "https://enterprise-ghes.com/api/v3"),
        (
            "https://github.com",
            "https://api.github.com",
        ),
    ],
)
async def test_get_head_sha_circle_ci(
    url: str,
    api_url: str,
    monkeypatch: pytest.MonkeyPatch,
    respx_mock: respx.MockRouter,
) -> None:
    monkeypatch.setenv(
        "CIRCLE_PULL_REQUESTS",
        f"{url}/owner/repo/pulls/123",
    )
    respx_mock.get(
        f"{api_url}/repos/owner/repo/pulls/123",
    ).respond(
        200,
        json={"head": {"sha": "ec26c3e57ca3a959ca5aad62de7213c562f8c821"}},
    )

    assert (
        await detector.get_circle_ci_head_sha()
        == "ec26c3e57ca3a959ca5aad62de7213c562f8c821"
    )


@pytest.mark.parametrize(
    ("url", "expected"),
    [
        ("https://github.com/owner/repo", "owner/repo"),
        ("https://github.com/owner/repo/", "owner/repo"),
        ("http://github.com/owner/repo", "owner/repo"),
        ("https://gitlab.com/owner/repo", "owner/repo"),
        ("https://git.example.com/owner/repo", "owner/repo"),
        ("owner/repo", "owner/repo"),
        ("https://github.com/my-org.name/my-repo.name", "my-org.name/my-repo.name"),
        ("https://git.example.com:8080/owner/repo", "owner/repo"),
        ("https://github.com/owner123/repo456", "owner123/repo456"),
        ("git@github.com:owner/repo.git", "owner/repo"),
        ("git@github.com:owner/repo", "owner/repo"),
        ("git@gitlab.com:owner/repo.git", "owner/repo"),
        (
            "git@git.example.com:my-org.name/my-repo.name.git",
            "my-org.name/my-repo.name",
        ),
        ("git@bitbucket.org:owner123/repo456.git", "owner123/repo456"),
    ],
)
def test_get_repository_name_from_url_valid(
    monkeypatch: pytest.MonkeyPatch,
    url: str,
    expected: str,
) -> None:
    """Test valid URL formats that should extract repository names."""
    monkeypatch.setenv("MY_URL", url)
    result = detector._get_github_repository_from_env("MY_URL")
    assert result == expected


@pytest.mark.parametrize(
    "url",
    [
        "https://github.com/owner/repo/issues",
        "https://github.com/owner",
        "",
        "not-a-url",
        "https://github.com/owner/repo?tab=readme",
    ],
)
def test_get_repository_name_from_url_invalid(
    monkeypatch: pytest.MonkeyPatch,
    url: str,
) -> None:
    """Test invalid URL formats that should return None."""
    monkeypatch.setenv("MY_URL", url)
    result = detector._get_github_repository_from_env("MY_URL")
    assert result is None


def test_get_github_pull_request_number_github_actions(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("GITHUB_ACTIONS", "true")
    monkeypatch.setenv("GITHUB_EVENT_NAME", "pull_request")
    monkeypatch.setenv("GITHUB_EVENT_PATH", str(PULL_REQUEST_EVENT))

    result = detector.get_github_pull_request_number()
    assert result == 2


def test_get_github_pull_request_number_no_event(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("GITHUB_ACTIONS", "true")
    monkeypatch.delenv("GITHUB_EVENT_PATH", raising=False)

    result = detector.get_github_pull_request_number()
    assert result is None


def test_get_github_pull_request_number_non_pr_event(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    event_data = {"push": {"ref": "refs/heads/main"}}
    event_file = tmp_path / "push_event.json"
    event_file.write_text(json.dumps(event_data))

    monkeypatch.setenv("GITHUB_ACTIONS", "true")
    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event_file))

    result = detector.get_github_pull_request_number()
    assert result is None


def test_get_github_pull_request_number_unsupported_ci(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("GITHUB_ACTIONS", raising=False)
    monkeypatch.delenv("CIRCLECI", raising=False)

    result = detector.get_github_pull_request_number()
    assert result is None


def test_get_mergify_config_path_default_file(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.chdir(tmp_path)
    (tmp_path / ".mergify.yml").touch()

    result = detector.get_mergify_config_path()
    assert result == ".mergify.yml"


def test_get_mergify_config_path_mergify_dir(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.chdir(tmp_path)
    (tmp_path / ".mergify").mkdir()
    (tmp_path / ".mergify" / "config.yml").touch()

    result = detector.get_mergify_config_path()
    assert result == ".mergify/config.yml"


def test_get_mergify_config_path_github_dir(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.chdir(tmp_path)
    (tmp_path / ".github").mkdir()
    (tmp_path / ".github" / "mergify.yml").touch()

    result = detector.get_mergify_config_path()
    assert result == ".github/mergify.yml"


def test_get_mergify_config_path_priority(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Test that .mergify.yml takes priority over other locations."""
    monkeypatch.chdir(tmp_path)
    (tmp_path / ".mergify.yml").touch()
    (tmp_path / ".mergify").mkdir()
    (tmp_path / ".mergify" / "config.yml").touch()
    (tmp_path / ".github").mkdir()
    (tmp_path / ".github" / "mergify.yml").touch()

    result = detector.get_mergify_config_path()
    assert result == ".mergify.yml"


def test_get_mergify_config_path_none_when_missing(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Test that None is returned when no config file exists."""
    monkeypatch.chdir(tmp_path)

    result = detector.get_mergify_config_path()
    assert result is None
