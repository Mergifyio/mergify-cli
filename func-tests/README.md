# Functional tests

End-to-end tests that drive the real `mergify` binary against a local
mock HTTP server (`pytest-httpserver`). Unlike `compat-tests/` — which
exercise CLI argument parsing and exit-code contracts that fire before
any external call — functional tests cover commands that talk to the
Mergify API:

- `mergify ci scopes-send` — `POST /v1/repos/{owner}/{repo}/pulls/{n}/scopes`
- `mergify ci junit-process` — OTLP traces upload + quarantine check
- `mergify ci junit-upload` (deprecated alias of `junit-process`)

Runner: `func-tests/test_*.py` (pytest-discovered).

Invoke:

```bash
uv run poe func-test
# or: uv run pytest func-tests/
```

## How it works

Each test:

1. Starts a `pytest-httpserver` instance (real socket on `127.0.0.1`).
2. Registers expected request handlers (path, headers, body).
3. Invokes the `mergify` CLI as a subprocess pointed at that server
   via `--api-url` / `MERGIFY_API_URL`.
4. Asserts the subprocess exit code, optional stdout substrings, and
   that the mock received the expected request(s).

The `mergify` binary is the user-facing entry point — it dispatches
to ported Rust subcommands or shells back to `python -m mergify_cli`
for the rest. Running through the binary tests the real release
artifact end-to-end.

## Adding a test

- Drop a JUnit XML fixture under `fixtures/` if the test needs one.
- Use the `httpserver` fixture (provided by `pytest-httpserver`) to
  register expected requests and the `cli` fixture to invoke the
  binary with the right env scrubbed.
- Assert on `result.returncode`, request count via `httpserver`, and
  any user-visible stdout/stderr.

## Live smoke tests

`test_live_smoke.py` (marked `pytest.mark.live`) hits the real
Mergify API at `mergify-clients-testing/mergify-cli-repo` PR #1.
Skipped by default — runs in `.github/workflows/func-tests-live.yaml`
on a nightly cron and manual dispatch, never on PRs.

Run locally:

```bash
LIVE_TEST_MERGIFY_TOKEN=<app-key> uv run poe live-test
```

The mock-vs-live split exists because the mock alone can drift
silently from the real API. The live job is a canary: when it
fails, the mock contract has gotten out of sync.

## Why a real HTTP server (not respx)

`respx`/`responses` patch the in-process HTTP client. Functional
tests run the CLI in a subprocess (often a Rust binary), so the mock
must be reachable over a real socket — `pytest-httpserver` runs a
threaded HTTP server bound to a random localhost port, which works
for both Python and Rust callers.
