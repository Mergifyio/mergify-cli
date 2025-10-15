import json
import pathlib
from unittest import mock

import pytest
import respx
import yaml

from mergify_cli.ci.scopes import base_detector
from mergify_cli.ci.scopes import cli
from mergify_cli.ci.scopes import config


def test_from_yaml_with_extras_ignored(tmp_path: pathlib.Path) -> None:
    config_file = tmp_path / "config.yml"
    config_file.write_text(
        yaml.dump(
            {
                "defaults": {},
                "queue_rules": [],
                "pull_request_rules": [],
                "partitions_rules": [],
                "scopes": {
                    "source": {
                        "files": {
                            "backend": {"include": ["api/**/*.py", "backend/**/*.py"]},
                            "frontend": {"include": ["ui/**/*.js", "ui/**/*.tsx"]},
                            "docs": {"include": ["*.md", "docs/**/*"]},
                        },
                    },
                },
            },
        ),
    )

    cfg = config.Config.from_yaml(str(config_file))
    assert cfg.model_dump() == {
        "scopes": {
            "source": {
                "files": {
                    "backend": {
                        "include": ("api/**/*.py", "backend/**/*.py"),
                        "exclude": (),
                    },
                    "frontend": {
                        "include": ("ui/**/*.js", "ui/**/*.tsx"),
                        "exclude": (),
                    },
                    "docs": {"include": ("*.md", "docs/**/*"), "exclude": ()},
                },
            },
            "merge_queue_scope": "merge-queue",
        },
    }


def test_from_yaml_valid(tmp_path: pathlib.Path) -> None:
    config_file = tmp_path / "config.yml"
    config_file.write_text(
        yaml.dump(
            {
                "scopes": {
                    "source": {
                        "files": {
                            "backend": {"include": ["api/**/*.py", "backend/**/*.py"]},
                            "frontend": {"include": ["ui/**/*.js", "ui/**/*.tsx"]},
                            "docs": {"include": ["*.md", "docs/**/*"]},
                        },
                    },
                },
            },
        ),
    )

    cfg = config.Config.from_yaml(str(config_file))
    assert cfg.model_dump() == {
        "scopes": {
            "merge_queue_scope": "merge-queue",
            "source": {
                "files": {
                    "backend": {
                        "include": ("api/**/*.py", "backend/**/*.py"),
                        "exclude": (),
                    },
                    "frontend": {
                        "include": ("ui/**/*.js", "ui/**/*.tsx"),
                        "exclude": (),
                    },
                    "docs": {"include": ("*.md", "docs/**/*"), "exclude": ()},
                },
            },
        },
    }


def test_from_yaml_invalid_config(tmp_path: pathlib.Path) -> None:
    config_file = tmp_path / "config.yml"
    config_file.write_text(
        yaml.dump({"scopes": {"source": {"files": {"Back#end-API": ["api/**/*.py"]}}}}),
    )

    # Bad name and missing dict
    with pytest.raises(config.ConfigInvalidError, match="3 validation errors"):
        config.Config.from_yaml(str(config_file))


def test_match_scopes_basic() -> None:
    cfg = config.Config.from_dict(
        {
            "scopes": {
                "source": {
                    "files": {
                        "backend": {"include": ("api/**/*.py", "backend/**/*.py")},
                        "frontend": {"include": ("ui/**/*.js", "ui/**/*.tsx")},
                        "docs": {"include": ("*.md", "docs/**/*")},
                    },
                },
            },
        },
    )
    files = ["api/models.py", "ui/components/Button.tsx", "README.md", "other.txt"]

    assert cfg.scopes.source is not None
    assert isinstance(cfg.scopes.source, config.SourceFiles)
    scopes_hit, per_scope = cli.match_scopes(files, cfg.scopes.source.files)

    assert scopes_hit == {"backend", "frontend", "docs"}
    assert per_scope == {
        "backend": ["api/models.py"],
        "frontend": ["ui/components/Button.tsx"],
        "docs": ["README.md"],
    }


def test_match_scopes_no_matches() -> None:
    cfg = config.Config.from_dict(
        {
            "scopes": {
                "source": {
                    "files": {
                        "backend": {"include": ("api/**/*.py",)},
                        "frontend": {"include": ("ui/**/*.js",)},
                    },
                },
            },
        },
    )
    files = ["other.txt", "unrelated.cpp"]

    assert cfg.scopes.source is not None
    assert isinstance(cfg.scopes.source, config.SourceFiles)
    scopes_hit, per_scope = cli.match_scopes(files, cfg.scopes.source.files)

    assert scopes_hit == set()
    assert per_scope == {}


def test_match_scopes_multiple_include_single_scope() -> None:
    cfg = config.Config.from_dict(
        {
            "scopes": {
                "source": {
                    "files": {
                        "backend": {"include": ("api/**/*.py", "backend/**/*.py")},
                    },
                },
            },
        },
    )
    files = ["api/models.py", "backend/services.py"]

    assert cfg.scopes.source is not None
    assert isinstance(cfg.scopes.source, config.SourceFiles)
    scopes_hit, per_scope = cli.match_scopes(files, cfg.scopes.source.files)

    assert scopes_hit == {"backend"}
    assert per_scope == {
        "backend": ["api/models.py", "backend/services.py"],
    }


def test_match_scopes_with_negation_include() -> None:
    cfg = config.Config.from_dict(
        {
            "scopes": {
                "source": {
                    "files": {
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
            },
        },
    )
    files = [
        "api/models.py",
        "api/test_models.py",
        "ui/components.js",
        "ui/components.spec.js",
    ]

    assert cfg.scopes.source is not None
    assert isinstance(cfg.scopes.source, config.SourceFiles)
    scopes_hit, per_scope = cli.match_scopes(files, cfg.scopes.source.files)

    assert scopes_hit == {"backend", "frontend"}
    assert per_scope == {
        "backend": ["api/models.py"],  # test_models.py excluded by negation
        "frontend": ["ui/components.js"],  # components.spec.js excluded by negation
    }


def test_match_scopes_negation_only() -> None:
    cfg = config.Config.from_dict(
        {
            "scopes": {
                "source": {
                    "files": {
                        "exclude_images": {
                            "include": ("**/*",),
                            "exclude": ("**/*.jpeg", "**/*.png"),
                        },
                    },
                },
            },
        },
    )
    files = ["image.jpeg", "document.txt", "photo.png", "readme.md"]

    assert cfg.scopes.source is not None
    assert isinstance(cfg.scopes.source, config.SourceFiles)
    scopes_hit, per_scope = cli.match_scopes(files, cfg.scopes.source.files)

    assert scopes_hit == {"exclude_images"}
    assert per_scope == {
        "exclude_images": ["document.txt", "readme.md"],  # images excluded
    }


def test_match_scopes_mixed_with_complex_negation() -> None:
    cfg = config.Config.from_dict(
        {
            "scopes": {
                "source": {
                    "files": {
                        "backend": {
                            "include": ("**/*.py",),
                            "exclude": ("**/test_*.py", "**/*_test.py"),
                        },
                    },
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

    assert cfg.scopes.source is not None
    assert isinstance(cfg.scopes.source, config.SourceFiles)
    scopes_hit, per_scope = cli.match_scopes(files, cfg.scopes.source.files)

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


@mock.patch("mergify_cli.ci.scopes.cli.base_detector.detect")
@mock.patch("mergify_cli.ci.scopes.changed_files.git_changed_files")
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
            "source": {
                "files": {
                    "backend": {"include": ["api/**/*.py"]},
                    "frontend": {"include": ["api/**/*.js"]},
                },
            },
        },
    }
    config_file = tmp_path / "mergify-ci.yml"
    config_file.write_text(yaml.dump(config_data))

    # Setup mocks
    mock_detect_base.return_value = base_detector.Base("main", is_merge_queue=True)
    mock_git_changed.return_value = ["api/models.py", "other.txt"]

    # Capture output
    with mock.patch("click.echo") as mock_echo:
        result = cli.detect(str(config_file))

    # Verify calls
    mock_detect_base.assert_called_once()
    mock_git_changed.assert_called_once_with("main")
    mock_github_outputs.assert_called_once()

    # Verify output
    calls = [call.args[0] for call in mock_echo.call_args_list]
    assert "Base: main" in calls
    assert "Scopes touched:" in calls
    assert "- backend" in calls
    assert "- merge-queue" in calls

    assert result.base_ref == "main"
    assert result.scopes == {"backend", "merge-queue"}


@mock.patch("mergify_cli.ci.scopes.cli.base_detector.detect")
@mock.patch("mergify_cli.ci.scopes.changed_files.git_changed_files")
@mock.patch("mergify_cli.ci.scopes.cli.maybe_write_github_outputs")
def test_detect_no_matches(
    _: mock.Mock,
    mock_git_changed: mock.Mock,
    mock_detect_base: mock.Mock,
    tmp_path: pathlib.Path,
) -> None:
    # Setup config file
    config_data = {
        "scopes": {"source": {"files": {"backend": {"include": ["api/**/*.py"]}}}},
    }
    config_file = tmp_path / ".mergify-ci.yml"
    config_file.write_text(yaml.dump(config_data))

    # Setup mocks
    mock_detect_base.return_value = base_detector.Base("main", is_merge_queue=False)
    mock_git_changed.return_value = ["other.txt"]

    # Capture output
    with mock.patch("click.echo") as mock_echo:
        result = cli.detect(str(config_file))

    # Verify output
    calls = [call.args[0] for call in mock_echo.call_args_list]
    assert "Base: main" in calls
    assert "No scopes matched." in calls
    assert result.scopes == set()
    assert result.base_ref == "main"


@mock.patch("mergify_cli.ci.scopes.cli.base_detector.detect")
@mock.patch("mergify_cli.ci.scopes.changed_files.git_changed_files")
def test_detect_debug_output(
    mock_git_changed: mock.Mock,
    mock_detect_base: mock.Mock,
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # Setup debug environment
    monkeypatch.setenv("ACTIONS_STEP_DEBUG", "true")

    # Setup config file
    config_data = {
        "scopes": {"source": {"files": {"backend": {"include": ["api/**/*.py"]}}}},
    }
    config_file = tmp_path / ".mergify-ci.yml"
    config_file.write_text(yaml.dump(config_data))

    # Setup mocks
    mock_detect_base.return_value = base_detector.Base("main", is_merge_queue=False)
    mock_git_changed.return_value = ["api/models.py", "api/views.py"]

    # Capture output
    with mock.patch("click.echo") as mock_echo:
        result = cli.detect(str(config_file))

    # Verify debug output includes file details
    calls = [call.args[0] for call in mock_echo.call_args_list]
    assert any("    api/models.py" in call for call in calls)
    assert any("    api/views.py" in call for call in calls)

    assert result.base_ref == "main"
    assert result.scopes == {"backend"}


async def test_upload_scopes(respx_mock: respx.MockRouter) -> None:
    api_url = "https://api.mergify.test"
    token = "test-token"  # noqa: S105
    repository = "owner/repo"
    pull_request = 123

    # Mock the HTTP request
    route = respx_mock.post(
        f"{api_url}/v1/repos/{repository}/pulls/{pull_request}/scopes",
    ).mock(
        return_value=respx.MockResponse(200, json={"status": "ok"}),
    )

    # Call the upload function
    await cli.send_scopes(
        api_url,
        token,
        repository,
        pull_request,
        ["backend", "frontend"],
    )

    # Verify the request was made
    assert route.called
    assert route.call_count == 1

    # Verify the request body
    request = route.calls[0].request
    assert request.headers["Authorization"] == "Bearer test-token"
    assert request.headers["Accept"] == "application/json"

    # Verify the JSON payload
    payload = json.loads(request.content)
    assert payload == {"scopes": ["backend", "frontend"]}


def test_dump(tmp_path: pathlib.Path) -> None:
    config_file = tmp_path / "scopes.json"
    saved = cli.DetectedScope(base_ref="main", scopes={"backend", "merge-queue"})
    saved.save_to_file(str(config_file))

    loaded = cli.DetectedScope.load_from_file(str(config_file))
    assert loaded.scopes == saved.scopes
    assert loaded.base_ref == saved.base_ref
