# Jenkins

## The key

A Jenkins credential of kind *Secret text* with the ID `MERGIFY_TOKEN`,
exposed in the stage that runs the tests:

```groovy
stage('Test') {
    environment {
        MERGIFY_TOKEN = credentials('MERGIFY_TOKEN')
        // Jenkins does not set CI, and the test plugins only report when it is true.
        CI = 'true'
    }
    steps {
        sh 'pytest'
    }
}
```

Set these on the stage that runs the tests, not the whole pipeline. A missing
credential fails the stage: the user creates it before the change merges, and
the pull request says so.

- **`CI`**: Jenkins does not set it. When the tests behave differently under
  `CI` (grep them for it), set `MERGIFY_CI_ENABLE = 'true'` instead.
- **Job name**: tests are filed under `JOB_NAME`, as both the pipeline and
  the job name. A multibranch pipeline's builds are filed under the
  multibranch project instead (`my-repo`, not `my-repo/PR-123`), so every
  branch and pull request shares one test history. `MERGIFY_TEST_JOB_NAME`
  replaces the job name, not the pipeline name: set it as SKILL.md says (per
  `matrix` cell or `parallel` branch that runs the suite, per suite), never to
  merge two Jenkins jobs' histories.
- **Pull request builds**: tests are filed under the branch the pull request
  comes from (`CHANGE_BRANCH`) and its head commit, not `PR-123`. Under the
  merge strategy the head is read from the merge Jenkins made in the
  checkout: tests run outside the checkout are filed under that merge.

## JUnit jobs

Install the Mergify CLI on the agent with its install script (it installs
`mergify` to `~/.local/bin`: put that on `PATH`):

```bash
curl -fsSL https://raw.githubusercontent.com/Mergifyio/mergify-cli/main/install.sh | sh
```

Upload in the same `sh` step as the tests. Jenkins runs `sh` with
`/bin/sh -xe`, often `dash`, which has no `set -o pipefail`: start a script
that needs it with `#!/bin/bash`.

```groovy
sh '''#!/bin/bash
set -o pipefail
exit_code=0
<test command> || exit_code=$?
mergify ci junit-process --test-exit-code "$exit_code" junit.xml
'''
```

The Mergify Jenkins plugin (job metrics for CI Insights) is separate: it is
installed in Manage Jenkins, not in the repository.
