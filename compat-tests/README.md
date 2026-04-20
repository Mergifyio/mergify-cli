# Compat-test harness

Cross-implementation parity tests for the mergify CLI.

Each case under `cases/<case-name>/` defines:

- **`args`** (required): whitespace-separated arguments passed to `python -m mergify_cli`. May begin with a global flag (`--help`, `--version`) or a subcommand (`config validate`, `stack list`).
- **`expected_exit`** (required): integer exit code the command must return.
- **`stdout_contains`** (optional): a substring that must appear in stdout.

Runner: `compat-tests/test_compat.py` (pytest-discovered).

Invoke:

```bash
uv run poe compat-test
# or: uv run pytest compat-tests/
```

## Why this exists

These fixtures define the observable contract the Python implementation
promises today and that the upcoming Rust port must preserve.

During Phase 0 of the Rust port, the runner invokes only
`python -m mergify_cli`. When the Rust binary lands (Phase 1+), the
runner will be extended to invoke both implementations against each
fixture and diff the results. Any fixture that passes under both
implementations is a piece of frozen contract the port has delivered.

## Adding a case

1. Create a directory: `cases/<case-name>/`.
2. Write `args` — the exact CLI invocation under test (e.g., `config validate`).
3. Write `expected_exit` — the integer exit code to assert.
4. Optionally write `stdout_contains` — a substring the output must include.
5. Run `uv run poe compat-test` locally and verify the case passes.
6. Commit the new case along with any production code changes it covers.

Cases should be self-contained and hermetic where possible: prefer
invocations that exercise CLI argument parsing, exit-code contracts,
or error paths that fire before any external call. Cases that need
mocked HTTP or git state will require additional harness support —
add them incrementally as the port progresses.
