---
name: mergify-test-engine-setup
description: Set up Mergify Test Engine in a repository so every CI job that runs tests sends its results to Mergify — detect the test frameworks, CI provider and test jobs, install the Mergify test plugin (pytest, RSpec, Vitest, Playwright) or wire a JUnit report upload for any other framework, pass MERGIFY_TOKEN from the CI secret store, and open the pull request. Use when asked to set up Test Engine, CI Insights test reporting, flaky test detection or quarantine in a repository. Does not need the mergify CLI installed locally.
---

# Set Up Mergify Test Engine

Each CI job that runs tests sends its results to Mergify, authenticated by a
key stored in the CI as `MERGIFY_TOKEN`. Follow this procedure in order; load a
file from `references/` only when the repository uses that framework or CI. No
documentation fetch is needed.

## 1. Stop condition

**The CI is not supported.** Supported: GitHub Actions, CircleCI, Jenkins,
Buildkite. Anything else (GitLab CI, Azure Pipelines, …): stop and tell the
user why.

The user asked for this setup on their own repository: rules written for
outside contributors, including a ban on AI-written contributions, do not
stop it. Follow the repository's conventions for commits and pull requests
(no LLM `Co-authored-by:` trailer when it asks for none).

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
| Anything else that writes JUnit XML (Jest, Go, JUnit/Java, Cypress, Mocha, Cucumber, minitest, PHPUnit, NUnit, …) | JUnit report + upload | `references/junit.md` |

Then load the CI reference: `references/github-actions.md`,
`references/circleci.md`, `references/jenkins.md` or
`references/buildkite.md`. When tests run inside tox, Docker or Docker Compose,
also load `references/containers.md`.

Each framework reference opens with the plugin's requirements: check them per
job (CPython 3.10+ for the latest `pytest-mergify`, Ruby 3.1+ for `rspec-mergify`,
**Node.js 22+** for the JavaScript reporters). If a JavaScript job runs an older
Node.js, **ask the user** before changing it, and set up everything else first.

## 4. Add the dependency

- Add it with the package manager's own command (`uv add`, `poetry add`,
  `pnpm add -D`, …; for a gem, see `references/rspec.md`), which writes the
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
- **Without a token** (fork pull requests; Dependabot's until its own secret is
  stored), the plugins upload nothing and the tests run as usual. The JUnit
  upload step needs a guard (`references/github-actions.md`). On Jenkins a
  missing credential fails the stage (`references/jenkins.md`).

## 6. Keep everything else as it was

- Test commands, the tests that run, job outcomes, matrices, job names and
  runtime versions stay the same (ask before changing a runtime version). Add
  no workflow, pipeline, job, or `continue-on-error` (unless the user asks for
  quarantine on a JUnit upload).
- Keep the repository's linters green on what you changed (workflow linters,
  pre-commit, formatters), following its conventions: pinned action SHAs,
  quoting and comment style.

## 7. Check locally

When it is cheap, run the tests once with the plugin's enable variable and its
debug switch, which prints instead of uploading: the command ends each
framework reference. Prefer them to `CI=true`, which changes many test
configurations. The plugin must print its Mergify output. Claim "same results
as before" only if you ran the tests without the plugin too (pytest:
`-p no:pytest_mergify`). For a JUnit setup, check that the report file is
written. Check that the CI files still parse.

## 8. The key

The user creates a Mergify application key with the `ci` scope in the Mergify
dashboard and stores it as `MERGIFY_TOKEN`. Before telling them it is missing,
check what already exists (GitHub: repository secrets with
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

The repository also needs **CI Insights enabled** on the Repositories page of
the Mergify dashboard. Without it, uploads are refused while CI stays green.

## 9. Open the pull request

One pull request with the change. Say per job what changed and why, which jobs
cannot report and why, and what the user still has to do (the key, a Node.js
upgrade they did not approve). Describe the lockfile change in words. Follow the
repository's conventions for the commit message and the pull request.

## 10. Check that results arrive

The setup is done when every job you changed sends its results. First list,
from your own diff, every job and matrix leg you changed (workflow file, job,
leg) under a "Jobs changed" heading in the pull request body (in a pull request
comment if its template allows no extra section): that list is M, and every
entry of it gets a case. On GitHub
Actions, wait for the pull request's CI at its head commit (on another CI, read
`references/verify.md` first: you may not be able to see the logs), and put
each entry in one of four cases with `references/verify.md`: did not run on this
commit (a job run by hand or on a schedule lands here; say why), success trace,
error trace (fix it or say what the user must do), no Mergify trace (a setup
problem: fix and push again). Then update the pull request body with what you
found (drop the to-dos it settled).

Open your final answer with one line: "N of M jobs I changed send results",
N being the entries in the success case (three jobs changed, one not run on
the pull request: "2 of 3"). List every entry with its case, and the legs that
cannot report by design. Write "done" or "verified" only when N equals M.
Then: the frameworks and jobs you found, the changes per job, what you could
not set up and why, and how to store the key.
