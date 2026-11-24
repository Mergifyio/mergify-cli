# GitHub Actions

A job reaches Mergify with its ID token (OIDC) or with the `MERGIFY_TOKEN`
secret: SKILL.md, step 8, decides which. The sections below give the key's
steps; with OIDC, follow [OIDC](#oidc) instead wherever they pass the token.

## Plugin jobs (pytest, RSpec, Vitest, Playwright)

With a key, add the token to the step that runs the tests. `CI` is already set.

```yaml
- name: Run tests
  env:
    MERGIFY_TOKEN: ${{ secrets.MERGIFY_TOKEN }}
  run: pytest
```

- **Matrix**: every leg shares one job id, so set a name per leg from the
  matrix values on the same step:
  `MERGIFY_TEST_JOB_NAME: test (${{ matrix.python-version }}, ${{ matrix.os }})`.
- **Two test runs in one job**: a distinct `MERGIFY_TEST_JOB_NAME` on each step.
- **Shards**: one name for all shards.
- **Pull requests from forks and Dependabot** get no Actions secret (Dependabot
  has its own store, see "The key"): the plugins upload nothing and the job is
  unaffected. No guard needed.

## JUnit upload jobs

Give the test step an `id`, and upload after it with
`mergifyio/gha-mergify-ci` (latest major: `v27`; check
`gh release list -R Mergifyio/gha-mergify-ci --limit 1` for a newer one, and
pin it to a commit SHA if the repository pins its actions). With a key:

```yaml
jobs:
  test:
    env:
      HAS_MERGIFY_TOKEN: ${{ secrets.MERGIFY_TOKEN != '' }}
    steps:
      # ...
      - name: Run tests
        id: tests
        run: npm test
      - name: Mergify CI Upload
        if: (success() || failure()) && env.HAS_MERGIFY_TOKEN == 'true' && hashFiles('junit.xml') != ''
        uses: mergifyio/gha-mergify-ci@v27
        with:
          token: ${{ secrets.MERGIFY_TOKEN }}
          report_path: junit.xml
          test_step_outcome: ${{ steps.tests.outcome }}
```

- `if: (success() || failure())` uploads even when the tests fail. Write the
  whole condition, parentheses included: `&&` binds tighter than `||`.
  `hashFiles` (same path as `report_path`) skips the upload when an earlier
  step failed before the report was written.
- **The upload step fails without a token** (it exits 8 on pull requests from
  forks and from Dependabot): the `HAS_MERGIFY_TOKEN` guard skips it there.
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
    if: steps.tests.outcome == 'failure' && env.HAS_MERGIFY_TOKEN != 'true'
    run: exit 1
  ```

- The action runs on Linux, macOS and Windows runners.

## Reusable workflows

A workflow called with `uses: ./.github/workflows/<file>.yml` gets no secret
unless the caller passes it: `secrets: inherit`, or

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

A called workflow reports under its caller's workflow name: two called
workflows with the same job id (often `test`) collide, so set a distinct
`MERGIFY_TEST_JOB_NAME` (or `job_name`) in each.

## OIDC

The job trades its GitHub ID token for a Mergify token, with no secret:

- **Grant `id-token: write`** to the job (or the whole workflow). A
  `permissions` block sets every permission it does not list to `none`: keep
  the ones the job's other steps need, `contents: read` for
  `actions/checkout` at least.

  ```yaml
  jobs:
    test:
      permissions:
        id-token: write
        contents: read
  ```

- **No `MERGIFY_TOKEN`** on the test step, no `token` input on
  `mergifyio/gha-mergify-ci`, no `--token`: the plugins and `mergify ci`
  (2026.11.2 or later) and `mergifyio/gha-mergify-ci` (its latest major)
  exchange the ID token themselves. A set `MERGIFY_TOKEN` wins over the
  exchange; an empty one counts as unset.
- **JUnit upload**: fork pull requests get no ID token, so skip the upload
  there in place of the `HAS_MERGIFY_TOKEN` guard (drop that `env` too):

  ```yaml
  - name: Mergify CI Upload
    if: (success() || failure()) && !github.event.pull_request.head.repo.fork && hashFiles('junit.xml') != ''
    uses: mergifyio/gha-mergify-ci@<latest major>
    with:
      report_path: junit.xml
      test_step_outcome: ${{ steps.tests.outcome }}
  ```

  With `continue-on-error` on the test step, the step that fails the job
  runs on `steps.tests.outcome == 'failure' && github.event.pull_request.head.repo.fork`.
- **Reusable workflows** get at most what their caller grants: grant
  `id-token: write` on the calling job, and list it in a called job that
  declares its own `permissions`. Pass no secret.
- **Tests in tox or a container**: pass `ACTIONS_ID_TOKEN_REQUEST_URL` and
  `ACTIONS_ID_TOKEN_REQUEST_TOKEN` through instead of `MERGIFY_TOKEN`
  (`containers.md`).
- **Dependabot's pull requests** get an ID token: no Dependabot secret.
- **A refused job** prints Mergify's reason after
  `Error: Mergify refused the job's ID token:`, naming its exchange
  (`mergify-ci` skill, OIDC Trust Policies, says how to read it).

## Workflow linters

Some zizmor versions and personas report `secrets-outside-env` on the new
`secrets.MERGIFY_TOKEN` line. Run the repository's own zizmor configuration on
the changed workflow; if it reports the finding, follow the convention the
repository already uses for it (a deployment environment on the job, or an
inline `# zizmor: ignore[secrets-outside-env]`). Keep actionlint green too.

## The key

Only without OIDC. `gh secret set MERGIFY_TOKEN --repo <owner>/<repository>` (prompts for the
value). An organization secret covers every repository: check
`gh api repos/<owner>/<repository>/actions/organization-secrets --jq '.secrets[].name'`
before saying it is missing. Dependabot sees neither: when
`.github/dependabot.yml` exists, also give
`gh secret set MERGIFY_TOKEN --app dependabot --repo <owner>/<repository>`
unless `gh secret list --app dependabot` already lists it.
