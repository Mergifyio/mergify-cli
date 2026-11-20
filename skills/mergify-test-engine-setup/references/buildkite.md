# Buildkite

## The key

`MERGIFY_TOKEN` comes from the agents' environment: Buildkite secrets, a
secrets plugin backed by your own store, or an agent `environment` hook. **Its
value never goes into the pipeline file**: `buildkite-agent pipeline upload`
interpolates `${MERGIFY_TOKEN}` before storing the pipeline, so a value written
in an `env:` block is uploaded in plain text. Naming the variable (in a plugin's
`env` list, or under `secrets:`) is fine.

## Plugin jobs (pytest, RSpec, Vitest, Playwright)

No step changes when the step runs on the agent: Buildkite sets `CI` and the
`BUILDKITE_*` variables. The job name is the step label.

**Docker Compose plugin**: the container sees only the variables listed in the
plugin's `env`. List every name the reporter reads, plus the token and `CI`
(names only):

```yaml
plugins:
  - docker-compose#v5.4.1:
      run: app
      env:
        - MERGIFY_TOKEN
        - CI
        - BUILDKITE
        - BUILDKITE_REPO
        - BUILDKITE_BRANCH
        - BUILDKITE_COMMIT
        - BUILDKITE_LABEL
        - BUILDKITE_STEP_KEY
        - BUILDKITE_BUILD_ID
        - BUILDKITE_BUILD_URL
        - BUILDKITE_RETRY_COUNT
        - BUILDKITE_PIPELINE_SLUG
        - BUILDKITE_AGENT_NAME
        - BUILDKITE_PULL_REQUEST
        - BUILDKITE_PULL_REQUEST_BASE_BRANCH
```

Add `MERGIFY_TEST_JOB_NAME` if you set it. Keep the plugin version the
pipeline already uses.

## JUnit jobs

Use the `mergifyio/mergify-ci` Buildkite plugin (latest major `v8`): it runs
in the `post-command` hook, so it uploads after the tests, even when they fail,
and reads the step's exit code itself.

```yaml
steps:
  - label: "Run tests"
    command: <test command>
    plugins:
      - mergifyio/mergify-ci#v8:
          action: junit-process
          report_path: junit.xml
```

Matrix steps already report distinct labels; set `job_name` only to file the
reports under another name.
