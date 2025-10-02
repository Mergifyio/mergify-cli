import json
import pathlib
import subprocess
from unittest import mock

import click
import pytest
import yaml

from mergify_cli.ci.scopes import cli


def test_from_yaml_valid(tmp_path: pathlib.Path) -> None:
    config_file = tmp_path / "config.yml"
    config_file.write_text(
        yaml.dump(
            {
                "scopes": {
                    "backend": {"include": ["api/**/*.py", "backend/**/*.py"]},
                    "frontend": {"include": ["ui/**/*.js", "ui/**/*.tsx"]},
                    "docs": {"include": ["*.md", "docs/**/*"]},
                },
            },
        ),
    )

    config = cli.Config.from_yaml(str(config_file))
    assert config.model_dump() == {
        "scopes": {
            "backend": {"include": ("api/**/*.py", "backend/**/*.py"), "exclude": ()},
            "frontend": {"include": ("ui/**/*.js", "ui/**/*.tsx"), "exclude": ()},
            "docs": {"include": ("*.md", "docs/**/*"), "exclude": ()},
        },
    }


def test_from_yaml_invalid_config(tmp_path: pathlib.Path) -> None:
    config_file = tmp_path / "config.yml"
    config_file.write_text(yaml.dump({"scopes": {"Back#end-API": ["api/**/*.py"]}}))

    # Bad name and missing dict
    with pytest.raises(cli.ConfigInvalidError, match="2 validation errors"):
        cli.Config.from_yaml(str(config_file))


@mock.patch("mergify_cli.ci.scopes.cli.subprocess.check_output")
def test_git_changed_files(mock_subprocess: mock.Mock) -> None:
    mock_subprocess.return_value = "file1.py\nfile2.js\n"

    result = cli.git_changed_files("main")

    mock_subprocess.assert_called_once_with(
        ["git", "diff", "--name-only", "--diff-filter=ACMRTD", "main...HEAD"],
        text=True,
        encoding="utf-8",
    )
    assert result == ["file1.py", "file2.js"]


@mock.patch("mergify_cli.ci.scopes.cli.subprocess.check_output")
def test_git_changed_files_empty(mock_subprocess: mock.Mock) -> None:
    mock_subprocess.return_value = ""

    result = cli.git_changed_files("main")

    assert result == []


@mock.patch("mergify_cli.ci.scopes.cli.subprocess.check_output")
def test_run_command_failure(mock_subprocess: mock.Mock) -> None:
    mock_subprocess.side_effect = subprocess.CalledProcessError(1, ["git", "diff"])

    with pytest.raises(click.ClickException, match="Command failed"):
        cli._run(["git", "diff"])


def test_detect_base_github_base_ref(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("GITHUB_BASE_REF", "main")
    monkeypatch.delenv("GITHUB_EVENT_PATH", raising=False)

    result = cli.detect_base()

    assert result == "main"


def test_detect_base_from_event_path(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: pathlib.Path,
) -> None:
    event_data = {
        "pull_request": {
            "base": {"sha": "abc123"},
        },
    }
    event_file = tmp_path / "event.json"
    event_file.write_text(json.dumps(event_data))

    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event_file))
    monkeypatch.delenv("GITHUB_BASE_REF", raising=False)

    result = cli.detect_base()

    assert result == "abc123"


def test_detect_base_merge_queue_override(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: pathlib.Path,
) -> None:
    event_data = {
        "pull_request": {
            "title": "merge-queue: Merge group",
            "body": "```yaml\nchecking_base_sha: xyz789\n```",
            "base": {"sha": "abc123"},
        },
    }
    event_file = tmp_path / "event.json"
    event_file.write_text(json.dumps(event_data))

    monkeypatch.setenv("GITHUB_EVENT_PATH", str(event_file))

    result = cli.detect_base()

    assert result == "xyz789"


def test_detect_base_no_info(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("GITHUB_EVENT_PATH", raising=False)
    monkeypatch.delenv("GITHUB_BASE_REF", raising=False)

    with pytest.raises(click.ClickException, match="Could not detect base SHA"):
        cli.detect_base()


def test_yaml_docs_from_fenced_blocks_valid() -> None:
    body = """Some text
```yaml
---
checking_base_sha: xyz789
pull_requests: [{"number": 1}]
previous_failed_batches: []
...
```
More text"""

    result = cli._yaml_docs_from_fenced_blocks(body)

    assert result == cli.MergeQueueMetadata(
        {
            "checking_base_sha": "xyz789",
            "pull_requests": [{"number": 1}],
            "previous_failed_batches": [],
        },
    )


def test_yaml_docs_from_fenced_blocks_no_yaml() -> None:
    body = "No yaml here"

    result = cli._yaml_docs_from_fenced_blocks(body)

    assert result is None


def test_yaml_docs_from_fenced_blocks_empty_yaml() -> None:
    body = """Some text
```yaml
```
More text"""

    result = cli._yaml_docs_from_fenced_blocks(body)

    assert result is None


def test_match_scopes_basic() -> None:
    config = cli.Config.from_dict(
        {
            "scopes": {
                "backend": {"include": ("api/**/*.py", "backend/**/*.py")},
                "frontend": {"include": ("ui/**/*.js", "ui/**/*.tsx")},
                "docs": {"include": ("*.md", "docs/**/*")},
            },
        },
    )
    files = ["api/models.py", "ui/components/Button.tsx", "README.md", "other.txt"]

    scopes_hit, per_scope = cli.match_scopes(config, files)

    assert scopes_hit == {"backend", "frontend", "docs"}
    assert per_scope == {
        "backend": ["api/models.py"],
        "frontend": ["ui/components/Button.tsx"],
        "docs": ["README.md"],
    }


def test_match_scopes_no_matches() -> None:
    config = cli.Config.from_dict(
        {
            "scopes": {
                "backend": {"include": ("api/**/*.py",)},
                "frontend": {"include": ("ui/**/*.js",)},
            },
        },
    )
    files = ["other.txt", "unrelated.cpp"]

    scopes_hit, per_scope = cli.match_scopes(config, files)

    assert scopes_hit == set()
    assert per_scope == {}


def test_match_scopes_multiple_include_single_scope() -> None:
    config = cli.Config.from_dict(
        {
            "scopes": {
                "backend": {"include": ("api/**/*.py", "backend/**/*.py")},
            },
        },
    )
    files = ["api/models.py", "backend/services.py"]

    scopes_hit, per_scope = cli.match_scopes(config, files)

    assert scopes_hit == {"backend"}
    assert per_scope == {
        "backend": ["api/models.py", "backend/services.py"],
    }


def test_match_scopes_with_negation_include() -> None:
    config = cli.Config.from_dict(
        {
            "scopes": {
                "backend": {
                    "include": ("api/**/*.py",),
                    "exclude": ("api/**/test_*.py",),
                },
                "frontend": {
                    "include": ("ui/**/*.js",),
                    "exclude": ("ui/**/*.spec.js",),
                },
            },
        },
    )
    files = [
        "api/models.py",
        "api/test_models.py",
        "ui/components.js",
        "ui/components.spec.js",
    ]

    scopes_hit, per_scope = cli.match_scopes(config, files)

    assert scopes_hit == {"backend", "frontend"}
    assert per_scope == {
        "backend": ["api/models.py"],  # test_models.py excluded by negation
        "frontend": ["ui/components.js"],  # components.spec.js excluded by negation
    }


def test_match_scopes_negation_only() -> None:
    config = cli.Config.from_dict(
        {
            "scopes": {
                "exclude_images": {
                    "include": ("**/*",),
                    "exclude": ("**/*.jpeg", "**/*.png"),
                },
            },
        },
    )
    files = ["image.jpeg", "document.txt", "photo.png", "readme.md"]

    scopes_hit, per_scope = cli.match_scopes(config, files)

    assert scopes_hit == {"exclude_images"}
    assert per_scope == {
        "exclude_images": ["document.txt", "readme.md"],  # images excluded
    }


def test_match_scopes_mixed_with_complex_negation() -> None:
    config = cli.Config.from_dict(
        {
            "scopes": {
                "backend": {
                    "include": ("**/*.py",),
                    "exclude": ("**/test_*.py", "**/*_test.py"),
                },
            },
        },
    )
    files = [
        "src/models.py",
        "src/test_models.py",
        "src/models_test.py",
        "tests/integration_test.py",
        "main.py",
    ]

    scopes_hit, per_scope = cli.match_scopes(config, files)

    assert scopes_hit == {"backend"}
    assert per_scope == {
        "backend": ["src/models.py", "main.py"],  # test files excluded
    }


def test_maybe_write_github_outputs(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    output_file = tmp_path / "github_output"
    monkeypatch.setenv("GITHUB_OUTPUT", str(output_file))

    all_scopes = ["backend", "frontend", "docs"]
    scopes_hit = {"backend", "docs"}

    cli.maybe_write_github_outputs(all_scopes, scopes_hit)

    content = output_file.read_text()
    assert "scope_backend=true\n" in content
    assert "scope_docs=true\n" in content
    assert "scope_frontend=false\n" in content


def test_maybe_write_github_outputs_no_env(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("GITHUB_OUTPUT", raising=False)

    # Should not raise any exception
    cli.maybe_write_github_outputs(["backend"], {"backend"})


@mock.patch("mergify_cli.ci.scopes.cli.detect_base")
@mock.patch("mergify_cli.ci.scopes.cli.git_changed_files")
@mock.patch("mergify_cli.ci.scopes.cli.maybe_write_github_outputs")
def test_detect_with_matches(
    mock_github_outputs: mock.Mock,
    mock_git_changed: mock.Mock,
    mock_detect_base: mock.Mock,
    tmp_path: pathlib.Path,
) -> None:
    # Setup config file
    config_data = {
        "scopes": {
            "backend": {"include": ["api/**/*.py"]},
            "frontend": {"include": ["api/**/*.js"]},
        },
    }
    config_file = tmp_path / "mergify-ci.yml"
    config_file.write_text(yaml.dump(config_data))

    # Setup mocks
    mock_detect_base.return_value = "main"
    mock_git_changed.return_value = ["api/models.py", "other.txt"]

    # Capture output
    with mock.patch("click.echo") as mock_echo:
        cli.detect(str(config_file))

    # Verify calls
    mock_detect_base.assert_called_once()
    mock_git_changed.assert_called_once_with("main")
    mock_github_outputs.assert_called_once()

    # Verify output
    calls = [call.args[0] for call in mock_echo.call_args_list]
    assert "Base: main" in calls
    assert "Scopes touched:" in calls
    assert "- backend" in calls


@mock.patch("mergify_cli.ci.scopes.cli.detect_base")
@mock.patch("mergify_cli.ci.scopes.cli.git_changed_files")
@mock.patch("mergify_cli.ci.scopes.cli.maybe_write_github_outputs")
def test_detect_no_matches(
    _: mock.Mock,
    mock_git_changed: mock.Mock,
    mock_detect_base: mock.Mock,
    tmp_path: pathlib.Path,
) -> None:
    # Setup config file
    config_data = {"scopes": {"backend": {"include": ["api/**/*.py"]}}}
    config_file = tmp_path / ".mergify-ci.yml"
    config_file.write_text(yaml.dump(config_data))

    # Setup mocks
    mock_detect_base.return_value = "main"
    mock_git_changed.return_value = ["other.txt"]

    # Capture output
    with mock.patch("click.echo") as mock_echo:
        cli.detect(str(config_file))

    # Verify output
    calls = [call.args[0] for call in mock_echo.call_args_list]
    assert "Base: main" in calls
    assert "No scopes matched." in calls


@mock.patch("mergify_cli.ci.scopes.cli.detect_base")
@mock.patch("mergify_cli.ci.scopes.cli.git_changed_files")
def test_detect_debug_output(
    mock_git_changed: mock.Mock,
    mock_detect_base: mock.Mock,
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # Setup debug environment
    monkeypatch.setenv("ACTIONS_STEP_DEBUG", "true")

    # Setup config file
    config_data = {"scopes": {"backend": {"include": ["api/**/*.py"]}}}
    config_file = tmp_path / ".mergify-ci.yml"
    config_file.write_text(yaml.dump(config_data))

    # Setup mocks
    mock_detect_base.return_value = "main"
    mock_git_changed.return_value = ["api/models.py", "api/views.py"]

    # Capture output
    with mock.patch("click.echo") as mock_echo:
        cli.detect(str(config_file))

    # Verify debug output includes file details
    calls = [call.args[0] for call in mock_echo.call_args_list]
    assert any("    api/models.py" in call for call in calls)
    assert any("    api/views.py" in call for call in calls)
