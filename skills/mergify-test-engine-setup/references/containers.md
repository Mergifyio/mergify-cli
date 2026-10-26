# Tests inside tox, Docker or Docker Compose

tox, `docker run` and Docker Compose start the tests in an environment that
does not inherit the CI job's variables. Pass through `MERGIFY_TOKEN`, `CI`,
`MERGIFY_TEST_JOB_NAME` if you set it, and every variable the plugin reads to
identify the run. tox's `pass_env` accepts globs (`GITHUB_*`); Docker Compose
(`environment:`), `docker run -e` and the Buildkite Docker Compose plugin need
each name:

```text
GitHub Actions: GITHUB_ACTIONS GITHUB_BASE_REF GITHUB_EVENT_NAME GITHUB_EVENT_PATH
  GITHUB_HEAD_REF GITHUB_JOB GITHUB_REF_NAME GITHUB_REF_TYPE GITHUB_REPOSITORY
  GITHUB_REPOSITORY_ID GITHUB_RUN_ATTEMPT GITHUB_RUN_ID GITHUB_SERVER_URL
  GITHUB_SHA GITHUB_WORKFLOW RUNNER_NAME
CircleCI: CIRCLECI CIRCLE_BRANCH CIRCLE_BUILD_URL CIRCLE_JOB
  CIRCLE_REPOSITORY_URL CIRCLE_SHA1 CIRCLE_WORKFLOW_ID
Buildkite: BUILDKITE BUILDKITE_AGENT_NAME BUILDKITE_BRANCH BUILDKITE_BUILD_ID
  BUILDKITE_BUILD_URL BUILDKITE_COMMIT BUILDKITE_LABEL BUILDKITE_PIPELINE_SLUG
  BUILDKITE_PULL_REQUEST BUILDKITE_PULL_REQUEST_BASE_BRANCH BUILDKITE_REPO
  BUILDKITE_RETRY_COUNT BUILDKITE_STEP_KEY
Jenkins: JENKINS_URL BUILD_ID BUILD_URL JOB_NAME NODE_NAME GIT_URL GIT_BRANCH
  GIT_COMMIT CHANGE_TARGET
```

- Pass them by name (`- GITHUB_SHA` in a Compose `environment:` list, or
  `-e GITHUB_SHA`), never by writing a value into a file.
- **On GitHub Actions in a container**, also mount the file `GITHUB_EVENT_PATH`
  points to, at the same path: without it the run is recorded against the
  merge commit instead of the pull request's head commit.
- The image has no `.git`? The repository name comes from the variables above,
  which is why they must all be passed.
- Shards (`--shard`): every shard keeps the same job name.
