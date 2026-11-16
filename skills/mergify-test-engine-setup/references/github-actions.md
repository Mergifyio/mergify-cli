# GitHub Actions

A job reaches Mergify with its ID token (OIDC), with no secret to store. When
SKILL.md, step 8, picks a key instead, [With a key](#with-a-key) says what
changes.

## Grant the ID token

Grant `id-token: write` on each job you change, never on the whole workflow,
which would change the other jobs too.

```yaml
jobs:
  test:
    permissions:
      id-token: write
      contents: read
```

- A job's `permissions` block sets every permission it does not list to
  `none`, and replaces the workflow's own block. List what the job had before:
  the workflow's `permissions` entries if it has some, else what its steps use
  (`contents: read` for `actions/checkout`, `packages: read` for a `container:`
  or `services:` image from GHCR, `pull-requests: write` or `checks: write` for
  a step that comments or reports).
- Any code the job runs can then ask GitHub for an ID token, for any audience.
  When the repository's workflows already trust a cloud with their ID tokens
  (`aws-actions/configure-aws-credentials`, `google-github-actions/auth`,
  `azure/login`), say in the pull request that a cloud role trusting this
  repository's pull request or branch tokens now trusts the test jobs too.

The plugins and `mergify ci` (2026.11.2 or later) and
`mergifyio/gha-mergify-ci` (v30 or later) exchange the ID token themselves: no
`MERGIFY_TOKEN`, no `token` input, no `--token`. A set `MERGIFY_TOKEN` wins
over the exchange; an empty one counts as unset.

## Plugin jobs (pytest, RSpec, Vitest, Playwright)

Nothing to add on the step that runs the tests: `CI` is already set.

- **Matrix**: every leg shares one job id, so set a name per leg from the
  matrix values on the same step:
  `MERGIFY_TEST_JOB_NAME: test (${{ matrix.python-version }}, ${{ matrix.os }})`.
- **Two test runs in one job**: a distinct `MERGIFY_TEST_JOB_NAME` on each step.
- **Shards**: one name for all shards.
- **Pull requests from forks** get no ID token: the plugins upload nothing and
  the job is unaffected. No guard needed. Dependabot's pull requests get one.

## JUnit upload jobs

Give the test step an `id`, and upload after it with
`mergifyio/gha-mergify-ci` (latest major: `v30`; check
`gh release list -R Mergifyio/gha-mergify-ci --limit 1` for a newer one, and
pin it to a commit SHA if the repository pins its actions):

```yaml
jobs:
  test:
    permissions:
      id-token: write
      contents: read
    steps:
      # ...
      - name: Run tests
        id: tests
        run: npm test
      - name: Mergify Test Engine Upload
        id: upload
        if: (success() || failure()) && (!github.event.pull_request || github.event.pull_request.head.repo.full_name == github.repository) && hashFiles('junit.xml') != ''
        uses: mergifyio/gha-mergify-ci@v30
        with:
          report_path: junit.xml
          test_step_outcome: ${{ steps.tests.outcome }}
```

- `if: (success() || failure())` uploads even when the tests fail. Write the
  whole condition, parentheses included: `&&` binds tighter than `||`.
  `hashFiles` (same path as `report_path`) skips the upload when an earlier
  step failed before the report was written.
- **The upload step fails without a credential**: it exits 8 on pull requests
  from forks, which get no ID token. The second condition skips it on a pull
  request whose head is in another repository; `head.repo.fork` would also
  skip every pull request of a repository that is itself a fork. The step also
  fails while no trust policy matches the job (SKILL.md, step 8).
- `test_step_outcome` lets Mergify detect a runner that crashed while the report
  looks clean.
- **Matrix**: add `job_name: <name per leg>` to the action's inputs.
- **Two reports in one job** (two frameworks): one upload step each, with a
  distinct `job_name`.
- **No `continue-on-error`** on the test step unless the user asks for
  quarantine. With it, the job's result comes from the upload step, so also add
  a step that fails the job when the tests failed and nothing was uploaded:

  ```yaml
  - name: Fail when tests failed and no report was uploaded
    if: steps.tests.outcome == 'failure' && steps.upload.outcome == 'skipped'
    run: exit 1
  ```

- The action runs on Linux, macOS and Windows runners.

## Reusable workflows

A workflow called with `uses: ./.github/workflows/<file>.yml` gets at most the
permissions its caller grants: grant `id-token: write` on the calling job, and
list it in a called job that declares its own `permissions`. Pass no secret.

A called workflow reports under its caller's workflow name: two called
workflows with the same job id (often `test`) collide, so set a distinct
`MERGIFY_TEST_JOB_NAME` (or `job_name`) in each.

## Tests in tox or a container

Pass `ACTIONS_ID_TOKEN_REQUEST_URL` and `ACTIONS_ID_TOKEN_REQUEST_TOKEN`
through (`containers.md`): without them the plugin has no ID token to
exchange.

## A refused job

A job no trust policy matches prints Mergify's reason after
`Error: Mergify refused the job's ID token:`, naming its exchange
(`mergify-ci` skill, OIDC Trust Policies, says how to read it).

## With a key

The job reads the `MERGIFY_TOKEN` secret instead, and needs no
`id-token: write`.

- **Plugin jobs**: add the token to the step that runs the tests.

  ```yaml
  - name: Run tests
    env:
      MERGIFY_TOKEN: ${{ secrets.MERGIFY_TOKEN }}
    run: pytest
  ```

  Pull requests from forks and Dependabot get no Actions secret (Dependabot
  has its own store, below): the plugins upload nothing and the job is
  unaffected.
- **JUnit upload jobs**: pass the token, and skip the upload where there is
  none in place of the same-repository condition (the step exits 8 without
  one, on pull requests from forks and from Dependabot). The step that fails
  the job under `continue-on-error` stays as above:

  ```yaml
  jobs:
    test:
      env:
        HAS_MERGIFY_TOKEN: ${{ secrets.MERGIFY_TOKEN != '' }}
      steps:
        # ...
        - name: Mergify Test Engine Upload
          id: upload
          if: (success() || failure()) && env.HAS_MERGIFY_TOKEN == 'true' && hashFiles('junit.xml') != ''
          uses: mergifyio/gha-mergify-ci@v30
          with:
            token: ${{ secrets.MERGIFY_TOKEN }}
            report_path: junit.xml
            test_step_outcome: ${{ steps.tests.outcome }}
  ```

- **Reusable workflows** get no secret unless the caller passes it:
  `secrets: inherit`, or

  ```yaml
      secrets:
        MERGIFY_TOKEN: ${{ secrets.MERGIFY_TOKEN }}
  ```

  Passed by name, the called workflow must declare it, or the run fails:

  ```yaml
  on:
    workflow_call:
      secrets:
        MERGIFY_TOKEN:
          required: false
  ```

- **Workflow linters**: some zizmor versions and personas report
  `secrets-outside-env` on the new `secrets.MERGIFY_TOKEN` line. Run the
  repository's own zizmor configuration on the changed workflow; if it reports
  the finding, follow the convention the repository already uses for it (a
  deployment environment on the job, or an inline
  `# zizmor: ignore[secrets-outside-env]`). Keep actionlint green too.
- **Storing it**: `gh secret set MERGIFY_TOKEN --repo <owner>/<repository>`
  (prompts for the value). An organization secret covers every repository:
  check
  `gh api repos/<owner>/<repository>/actions/organization-secrets --jq '.secrets[].name'`
  before saying it is missing. Dependabot sees neither: when
  `.github/dependabot.yml` exists, also give
  `gh secret set MERGIFY_TOKEN --app dependabot --repo <owner>/<repository>`
  unless `gh secret list --app dependabot` already lists it.
