# pytest: `pytest-mergify`

The plugin loads through pytest's entry points: no test command or pytest
configuration change. It uploads at the end of the session.

## Requirements

- **CPython only.** Releases from 2026.10.6 need Python 3.10 or later;
  2026.10.5 is the last one for 3.8 and 3.9. When the project supports Python
  below 3.10, require `pytest-mergify>=2026.10.5` and let the resolver pick per
  interpreter. `pytest-mergify` ships compiled wheels and no source
  distribution: Linux (x86_64, aarch64; glibc and musl), macOS (x86_64, arm64),
  Windows (x86_64). There is no wheel for **PyPy** or **free-threaded** CPython
  (`3.13t`, `3.14t`); pip then falls back to an old release that compiles Rust,
  or fails.
- pytest 7.0 or later.

When a matrix has PyPy legs, add the requirement with an environment marker:

```text
pytest-mergify; platform_python_implementation == "CPython"
```

No marker tells a free-threaded build apart: keep the plugin out of those jobs
from the workflow (skip the install for those legs), and say in the pull
request which legs cannot report.

## Install

Add it where the project's test dependencies live, with its tool:

| Setup | Command or edit |
|---|---|
| pip requirements file | add `pytest-mergify` to the file CI installs from |
| uv | `uv add --dev pytest-mergify` (or `--group <name>`); with `add-bounds = "major"` in `pyproject.toml`, add `--bounds lower` (calendar versions: a major bound blocks next year's releases) |
| Poetry | `poetry add --group <test group> "pytest-mergify>=<latest version>"` (`>=2026.10.5` when the project supports Python below 3.10; a plain `poetry add` caps it at the next calendar year) |
| `[dependency-groups]` / extras / `setup.cfg` / `setup.py` | add it to the group or extra CI installs |

## CI

- GitHub Actions: `MERGIFY_TOKEN: ${{ secrets.MERGIFY_TOKEN }}` in the `env` of
  the step that runs pytest (see `github-actions.md`).
- Matrix: `MERGIFY_TEST_JOB_NAME` per leg from the matrix values.
- One job running pytest twice (for example with and without optional
  dependencies): a distinct `MERGIFY_TEST_JOB_NAME` on each step.
- `filterwarnings = error` in the pytest configuration is fine: the plugin
  raises no warning.
- No `continue-on-error`, no step `id`, no `test_step_outcome`: quarantine is
  applied inside pytest (a quarantined failing test becomes `xfailed`), so the
  exit code is already right.

## tox

tox does not pass the CI's variables to the tests, and does not pass `CI`.
Add the plugin to the test environment and pass the variables through:

```ini
[testenv]
deps =
    pytest-mergify
pass_env =
    CI
    MERGIFY_*
    GITHUB_*
    RUNNER_*
```

Use the same keys as the project (`deps`, `dependency_groups`, `extras`; old
projects use `passenv`). On another CI, replace `GITHUB_*` and `RUNNER_*` with
that CI's variables (`containers.md`). When one `tox` call runs several
environments (`py312-crypto`, `py312-nocrypto`), give each its own name:

```ini
set_env =
    MERGIFY_TEST_JOB_NAME = {env:MERGIFY_TEST_JOB_NAME:tox} ({envname})
```

## Variables

| Variable | Purpose |
|---|---|
| `MERGIFY_TOKEN` | the `ci` application key (from the CI secret store) |
| `MERGIFY_TEST_JOB_NAME` | the job name to report under |
| `MERGIFY_CI_ENABLE` | report even when `CI` is not set (was `PYTEST_MERGIFY_ENABLE`) |
| `MERGIFY_CI_DEBUG` | print the spans instead of uploading them (was `PYTEST_MERGIFY_DEBUG`) |

Check: `MERGIFY_CI_ENABLE=true MERGIFY_CI_DEBUG=true pytest …` prints a
`Mergify CI` section (on 2026.10.5, use the old names). Compare with
`pytest -p no:pytest_mergify …` for the run without the plugin.
