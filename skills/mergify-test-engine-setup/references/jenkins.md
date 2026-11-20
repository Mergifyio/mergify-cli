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
- **Job name**: the plugins file tests under `JOB_NAME`, as both the pipeline
  and the job name. In a multibranch pipeline it includes the branch or pull
  request (`my-repo/PR-123`), so each branch and pull request keeps its own
  test history, whatever you set: leave `MERGIFY_TEST_JOB_NAME` alone and tell
  the user. Set it only when a scheduled job and the pull request job have
  different Jenkins names but run the same tests.

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
