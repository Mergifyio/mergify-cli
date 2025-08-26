import pathlib

import pytest
import respx

from mergify_cli.ci import detector


PULL_REQUEST_EVENT = pathlib.Path(__file__).parent / "pull_request.json"


def test_get_head_branch_jenkins(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("GIT_BRANCH", "origin/main")

    assert detector.get_jenkins_head_ref_name() == "main"


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
