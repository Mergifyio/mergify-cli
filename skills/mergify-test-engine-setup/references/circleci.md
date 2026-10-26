# CircleCI

Only test results reach Mergify from CircleCI (no job metrics).

## The key

`MERGIFY_TOKEN` is never written in `.circleci/config.yml`. Where it goes is the
user's choice when they store it: do not ask. Set up for a project variable, and
say that a context also needs `context:` on the job.

- **Project environment variable**: Project Settings → Environment Variables,
  `https://app.circleci.com/settings/project/github/<owner>/<repository>/environment-variables`.
  Every job receives it; nothing changes in the configuration.
- **Or a context** (Organization Settings → Contexts), to share one key across
  projects: jobs receive it only when the workflow names it, so add
  `context: [<context name>]` to each test job under `workflows`.

Builds of pull requests from forks get neither, unless the project setting
*Pass secrets to builds from forked pull requests* is on.

## Plugin jobs (pytest, RSpec, Vitest, Playwright)

No step changes: CircleCI sets `CI` and the `CIRCLE_*` variables the plugin
reads.

- Each job reports under `CIRCLE_JOB`; matrix legs get their expanded names
  (`test-3.12`), already distinct. Parallel containers of one job
  (`parallelism`) share the name, which is right.
- The `docker` executor runs the steps in the container: variables reach them.
  When a step starts the tests itself with `docker run` or Docker Compose, load
  `containers.md`.
- CircleCI publishes no base branch for a pull request build: pull request
  builds are read like branch builds (Test Engine's pull request features do not
  apply).

## JUnit jobs

Install the Mergify CLI in the job with its install script (it installs
`mergify` to `~/.local/bin`: put that on `PATH`):

```bash
curl -fsSL https://raw.githubusercontent.com/Mergifyio/mergify-cli/main/install.sh | sh
```

Upload in the **same `run` step** as the tests: CircleCI runs steps with
`bash -eo pipefail`, so a failing test command on its own line would end the
step before the upload.

```bash
exit_code=0
<test command> || exit_code=$?
if [ -n "${MERGIFY_TOKEN:-}" ]; then
  mergify ci junit-process --test-exit-code "$exit_code" junit.xml
else
  exit "$exit_code"
fi
```

`junit-process` exits 0 when every failure it found is quarantined and 1
otherwise, so its status becomes the step's. The token guard keeps fork builds
on the tests' own exit code.
