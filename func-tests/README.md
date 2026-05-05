# Live functional tests

End-to-end smoke tests that drive the real `mergify` binary
against the real Mergify API at
`mergify-clients-testing/mergify-cli-repo` PR #1.

Coverage:

- `mergify config simulate` — `POST /v1/repos/{owner}/{repo}/pulls/{n}/simulator`
- `mergify ci scopes-send` — `POST /v1/repos/{owner}/{repo}/pulls/{n}/scopes`
- `mergify ci junit-process` — OTLP traces upload + quarantine check

Each test fires when the real API's URL, auth, or wire format
diverges from what the CLI expects. Asserts only "endpoint exists,
accepts our payload, returns 2xx" — never response content, since
the test tenant's state is not under test control.

## Running

CI: `.github/workflows/func-tests-live.yaml` runs nightly + on
manual dispatch. Not wired into the PR `ci-gate`, so an upstream
blip cannot block PRs.

Locally:

```bash
LIVE_TEST_MERGIFY_TOKEN=<app-key> uv run poe live-test
```

Skipped if `LIVE_TEST_MERGIFY_TOKEN` is unset.

## Adding a test

- Mark with `pytest.mark.live` (the module-level `pytestmark = pytest.mark.live`
  in `test_live_smoke.py` already does this).
- Use the `cli` fixture to invoke the binary and `live_token` to
  inject the token.
- Assert exit code only — never response content.
