---
name: mergify-test-engine-setup
description: Set up Mergify Test Engine in a repository so every CI job that runs tests sends its results to Mergify — detect the test frameworks, CI provider and test jobs, install the Mergify test plugin (pytest, RSpec, Minitest, Vitest, Playwright, Jest, Mocha) or wire a JUnit report upload for any other framework, let the jobs reach Mergify (GitHub Actions jobs with their ID token through an OIDC trust policy written with `mergify oidc`, else MERGIFY_TOKEN from the CI secret store), and open the pull request. Use when asked to set up Test Engine, CI Insights test reporting, flaky test detection, quarantine or test retry in a repository. Works without the mergify CLI installed locally, which then can neither write a trust policy nor check the results.
---

# Set Up Mergify Test Engine

Each CI job that runs tests sends its results to Mergify. A GitHub Actions job
can reach it with its own ID token, which a trust policy of the organization
allows (OIDC): no key to create, store or rotate. Any other job uses a key
stored in the CI as `MERGIFY_TOKEN`. Follow this procedure in order; load a
file from `references/` only when the repository uses that framework or CI. No
documentation fetch is needed.

## 1. Stop condition

**The CI is not supported** (supported: GitHub Actions, CircleCI, Jenkins,
Buildkite): stop and tell the user why. Rules written for outside contributors,
a ban on AI-written contributions included, do not stop this setup; follow the
repository's commit and pull request conventions (no LLM `Co-authored-by:`
trailer when it asks for none).

## 2. Detect

From the files, never from assumptions:

- **Every test framework**, with its version from the lockfile.
- **The CI provider**: `.github/workflows/`, `.circleci/`, `Jenkinsfile`,
  `.buildkite/`.
- **Every CI job that runs tests on pull requests.** Follow indirections to the
  real test command: `package.json` scripts, Makefile targets, tox or nox
  environments, Rake tasks, shell scripts, reusable workflows
  (`uses: ./.github/workflows/…`), Docker Compose services. For each job note its
  matrix, its runtime versions, and whether the tests run inside tox, Docker or
  a reusable workflow.
- **Jobs that are not this repository's tests** (lint, type checks, builds,
  docs, smoke tests without a test framework, a job that runs another
  project's test suite against this one): leave them alone. Leave out a job
  that reruns the suite many times over (once per time zone or locale): each
  run would upload. A job that only runs on a schedule or by hand is out of
  scope unless it runs the same tests as a pull request job.
- **Who opens pull requests**: forks, Dependabot (`.github/dependabot.yml`).

## 3. Pick the integration per job

| Framework | Integration | Load |
|---|---|---|
| pytest | `pytest-mergify` plugin | `references/pytest.md` |
| RSpec | `rspec-mergify` gem | `references/rspec.md` |
| Vitest 3+ | `@mergifyio/vitest` reporter | `references/vitest.md` |
| Playwright 1.56+ | `@mergifyio/playwright` reporter | `references/playwright.md` |
| Jest 29+ | `@mergifyio/jest` (`withMergify`) | `references/jest.md` |
| Mocha 8.2+ | `@mergifyio/mocha` (`require`) | `references/mocha.md` |
| Minitest 5.19+ | `minitest-mergify` gem | `references/minitest.md` |
| Anything else that writes JUnit XML (Go, JUnit/Java, Cypress, Cucumber, PHPUnit, NUnit, …) | JUnit report + upload | `references/junit.md` |

Then load the CI reference: `references/github-actions.md`,
`references/circleci.md`, `references/jenkins.md` or
`references/buildkite.md`. When tests run inside tox, Docker or Docker Compose,
also load `references/containers.md`.

Each framework reference opens with the plugin's requirements: check them per
job (CPython 3.10+ for the latest `pytest-mergify`, Ruby 3.1+ for the gems,
**Node.js 22+** for the JavaScript plugins). If a JavaScript job runs an older
Node.js, **ask the user** before changing it, and set up everything else first.

## 4. Add the dependency

- Add it with the package manager's own command (`uv add`, `poetry add`,
  `pnpm add -D`, …; for a gem, see its reference), which writes the
  manifest and the lockfile together. Never edit a lockfile by hand. With no
  lockfile, edit the file CI installs from (`requirements-dev.txt`,
  `pyproject.toml`, the gemspec, …).
- **Use the tool version the project uses**: `packageManager` in
  `package.json`, `mise.toml` or `.tool-versions`, the `BUNDLED WITH` line of
  `Gemfile.lock`. uv does not record its version in `uv.lock`: if the `revision`
  line or unrelated entries change, redo the change with another uv version
  (`uvx uv@<version>`) until they do not.
- **Check the result**: the lockfile's header is unchanged (uv `revision`,
  Poetry `lock-version`, `BUNDLED WITH`, npm and pnpm `lockfileVersion`, Yarn
  `__metadata`), and the diff, added and removed lines alike (read all of it),
  touches only the new packages and their dependencies. Older npm releases
  delete the `libc` fields of other packages: redo the install with
  `npx npm@latest install …`. Bundler dropping a `PLATFORMS` line: redo it
  with `bundle lock --add-platform …`. Never patch a lockfile by script. A
  change you cannot avoid (a pinned pnpm adding `libc:` lines to existing
  entries) you describe in words: which entries changed, and why. Do not quote
  line counts.
- **When you cannot resolve the lockfile** (a private registry you cannot
  reach), change the manifest only, say so in the pull request, and tell the
  user how to update the lockfile.
- Commit only what the change needs: no tool caches (`.yarn/install-state.gz`),
  nothing a local run wrote (`test-results/`, reports). Check `git status`.

## 5. Wire it into CI

The framework and CI references give the exact steps. The rules that apply
everywhere:

- **How the jobs reach Mergify** (step 8): on GitHub Actions, with their ID
  token (OIDC): each job is granted `id-token: write` and gets no
  `MERGIFY_TOKEN` (`references/github-actions.md`). On another CI, or when
  step 8 picks a key, with `MERGIFY_TOKEN`.
- **`MERGIFY_TOKEN` comes from the CI's secret store**, never from a file.
  Never ask for its value, never write a token anywhere.
- **The plugins report only when `CI` is `true`** (or their enable variable is
  set). GitHub Actions, CircleCI and Buildkite set `CI`; Jenkins does not. When
  tests behave differently under `CI` (grep the tests for it), set
  `MERGIFY_CI_ENABLE=true` instead (the old per-plugin names, such as
  `PYTEST_MERGIFY_ENABLE`, still work and print a deprecation line).
- **Job names.** A test is identified by its name and the name of the CI job
  that ran it, which defaults to the job's identifier in the CI. Set
  `MERGIFY_TEST_JOB_NAME`:
  - per matrix leg, from the matrix values (on GitHub Actions every leg shares
    one job id);
  - per step, when one job runs the tests twice (two suites, two tox
    environments);
  - per called workflow, when reusable workflows share a job id;
  - never per shard: shards of one suite share one name.
- **Without a credential** (pull requests from forks get no ID token and no
  secret; with a key, Dependabot's get none until its own secret is stored),
  the plugins upload nothing and the tests run as usual. The JUnit upload step
  needs a guard (`references/github-actions.md`). On Jenkins a missing
  credential fails the stage (`references/jenkins.md`).

## 6. Keep everything else as it was

- Test commands, the tests that run, job outcomes, matrices, job names and
  runtime versions stay the same (ask before changing a runtime version). Add
  no workflow, pipeline, job, or `continue-on-error` (unless the user asks for
  quarantine on a JUnit upload). The one other change a job may get is
  `MERGIFY_TEST_RETRY_ENABLE=false` in its environment, when the user asks to
  keep that job from retrying (step 9).
- Keep the repository's linters green on what you changed, following its
  conventions (pinned action SHAs, quoting, comment style).

## 7. Check locally

When it is cheap, run the tests once with the plugin's enable variable and its
debug switch, which prints instead of uploading: the command ends each
framework reference. Prefer them to `CI=true`, which changes many test
configurations. The plugin must print its Mergify output. Claim "same results
as before" only if you ran the tests without the plugin too (pytest:
`-p no:pytest_mergify`). For a JUnit setup, check that the report file is
written. Check that the CI files still parse.

## 8. How the jobs reach Mergify

**On GitHub Actions, OIDC.** A job granted `id-token: write` with no
`MERGIFY_TOKEN` trades its GitHub ID token for a short-lived Mergify OIDC CI
token, limited to its repository, when a trust policy of the organization
matches it: no secret to store, Dependabot's included. The plugins and
`mergify ci` need release 2026.11.2 or later (raise an older pin or lock), and
`mergifyio/gha-mergify-ci` v30 or later.
Until a policy matches, Mergify refuses the job's ID token: nothing is
uploaded or quarantined, the log warns, and the job passes or fails as its
tests do. A JUnit upload step does so from mergify-cli 2026.11.10, and fails
the job with an older CLI (`references/github-actions.md` says which CLI a
step runs). An organization admin or CI Admin writes the policy. Check with the `mergify` CLI
(`mergify-ci` skill, OIDC Trust Policies), signed in as the user
(`mergify auth status`; never log in for them):

1. `mergify oidc trust-policies list --org <owner>` shows each policy line by
   line, its repositories by name, or by visibility (`every internal or
   private repository`, which covers the repository when it has one of those
   visibilities, unless its jobs' ID tokens give none, as an older GitHub
   Enterprise Server's). A policy whose lines cover every job you changed
   (the repository, the events that start them, their workflows and
   environments): nothing to write.
2. No such policy: **ask the user before writing one**, since it decides which
   jobs of the organization get a token. Propose the narrowest that covers the
   jobs, usually
   `mergify oidc trust-policies create --org <owner> --name "<repository> CI" --repository <repository>`
   (CI events, any workflow, any environment), or
   `mergify oidc trust-policies update <id> --org <owner> --repository …` to
   add the repository to an existing one (list every repository it already
   holds: the flags replace the line; never on a policy by visibility, whose
   line it would replace: write a new policy instead). With their go, run it. HTTP 403: they
   are neither an admin nor a CI Admin: give them the command to hand to one.
3. Without the CLI, or when it cannot authenticate or read the policies, ask
   the user whether a trust policy of the organization covers the
   repository's GitHub Actions jobs. If none does, or they do not know, give
   them the `create` command above for an organization admin or CI Admin.

While no policy covers the jobs, say in the pull request that they send
nothing until it exists, with the command. When a JUnit upload step runs a
mergify-cli older than 2026.11.10, also open the pull request as a draft that
leaves draft once the policy exists: that step fails its job until then. A
user who would rather not wait uses a key.

**A key instead**, on any other CI, and on GitHub Actions when the user asks
for one rather than a trust policy: the user creates a Mergify application
key with the `ci` scope in the Mergify dashboard and stores it as
`MERGIFY_TOKEN`. Before telling them it is missing, check what already exists
(GitHub: repository secrets with
`gh secret list --repo <owner>/<repository>`, organization secrets with
`gh api repos/<owner>/<repository>/actions/organization-secrets --jq '.secrets[].name'`,
Dependabot's with `gh secret list --app dependabot --repo <owner>/<repository>`).
On GitHub Actions they store it with
`gh secret set MERGIFY_TOKEN --repo <owner>/<repository>` (it prompts for the
value). **If `.github/dependabot.yml` exists, your answer always gives**
`gh secret set MERGIFY_TOKEN --app dependabot --repo <owner>/<repository>`,
unless Dependabot's list already has it: Dependabot's pull requests run the
tests but never see Actions secrets, even organization ones. On another
CI, the CI reference's "The key" section says where it goes.

The repository also needs **Test Engine turned on** in the Mergify dashboard.
Without it, uploads are refused while CI stays green.

## 9. Test retry

With test retry, the plugin runs a failing test again in the same job, so a
flaky failure no longer fails the build. It needs a Mergify plugin (a JUnit
report upload never retries), and runs only on pushes to the default branch
and in runs the merge queue checks: release 2026.11.12 or later of the plugin
retries in the merge queue as well as on the default branch. Turn it on or off
only when the user asks.

- **For the repository**: `mergify tests retry on -r <owner>/<repository>`, or
  `off` (`show` says which it is). This needs none of the steps above. It
  needs write access to the repository, or an application key with the Test
  Retry Settings permission, and is available on plans that include
  quarantine.
- **For one job**: set `MERGIFY_TEST_RETRY_ENABLE=false` in that job's
  environment, the exception step 6 names.

## 10. Open the pull request

One pull request with the change. Say per job what changed and why, which jobs
cannot report and why, and what the user still has to do (the trust policy or
the key, a Node.js upgrade they did not approve). Describe the lockfile change in words. Follow the
repository's conventions for the commit message and the pull request.

## 11. Check that results arrive

The setup is done when every job you changed sends its results. First list,
from your own diff, every job and matrix leg you changed (workflow file, job,
leg) under a "Jobs changed" heading in the pull request body (in a pull request
comment if its template allows no extra section): that list is M, and every
entry of it gets a case. Then follow `references/verify.md`: ask Mergify what
it received for the pull request's head commit with `mergify tests runs` (any
CI; when the CLI is missing or cannot authenticate, read the CI logs instead),
and put each entry in one of four cases: did not run on this commit (a job run
by hand or on a schedule lands here; say why), results received, upload failed
(fix it or say what the user must do), nothing sent (a setup problem: fix and
push again). Then update the pull request body with what you found (drop the
to-dos it settled).

Open your final answer with one line: "N of M jobs I changed send results",
N being the entries whose results were received (three jobs changed, one not
run on the pull request: "2 of 3"). List every entry with its case, and the legs that
cannot report by design. Write "done" or "verified" only when N equals M.
Then: the frameworks and jobs you found, the changes per job, what you could
not set up and why, and how the jobs reach Mergify: the trust policy that
covers them, or how to store the key.
