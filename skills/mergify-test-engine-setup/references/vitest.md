# Vitest: `@mergifyio/vitest`

## Requirements

- **Node.js 22 or later** in the job that runs the tests. On an older Node.js,
  npm and pnpm only warn at install, Yarn 1 refuses, and at run time the
  reporter reports nothing and prints no Mergify line. If the
  job runs an older Node.js, ask the user before changing it, and set up
  everything else first.
- Vitest 3 or later.

## Install

`npm install --save-dev @mergifyio/vitest`, `pnpm add -D @mergifyio/vitest` or
`yarn add -D @mergifyio/vitest`, with the version of the package manager the
project pins (`packageManager`, `mise.toml`, `.tool-versions`). In a workspace,
add it to the package that runs the tests.

## Configure

Add the reporter to the file the Vitest settings already live in:
`vitest.config.*`, or the `test` section of `vite.config.*`. Do not create a
`vitest.config.*` next to a `vite.config.*` that holds them: Vitest then reads
only the new file and drops the existing settings. With no configuration file
at all, create `vitest.config.ts`.

```typescript
import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    reporters: [
      'default',
      ...(process.env.GITHUB_ACTIONS === 'true' ? ['github-actions'] : []),
      '@mergifyio/vitest',
    ],
  },
});
```

- **Setting `reporters` replaces Vitest's defaults.** When the configuration
  did not set `reporters`, keep them as above: `'default'` for the console, and
  `'github-actions'` on GitHub Actions for the failure annotations on pull
  requests. When it did, add the Mergify reporter to the existing list.
- Reference the reporter by package name, as above. Importing it and writing
  `new MergifyReporter()` breaks every suite when the configuration is loaded
  as CommonJS (no `"type": "module"`, not `.mts`): Vitest cannot then be
  `require`d.
- **`test.projects`**: every project reports under the same job name. Two
  projects running the same files (Node and browser) report each test twice
  under one identity. Do not split the CI job yourself: tell the user.
- No `continue-on-error`: quarantine is applied by a runner the reporter
  installs (not in browser mode, and not when the configuration sets its own
  `runner`; the reporter logs a warning then).

## CI

- GitHub Actions: `MERGIFY_TOKEN: ${{ secrets.MERGIFY_TOKEN }}` on the step
  that runs Vitest.
- Matrix: `MERGIFY_TEST_JOB_NAME` per leg (for example
  `test (node ${{ matrix.node-version }})`). `--shard` legs share one name.

## Variables

| Variable | Purpose |
|---|---|
| `MERGIFY_TOKEN` | the `ci` application key |
| `MERGIFY_TEST_JOB_NAME` | the job name to report under |
| `MERGIFY_CI_ENABLE` | report even when `CI` is not set (was `VITEST_MERGIFY_ENABLE`) |
| `MERGIFY_CI_DEBUG` | print the spans instead of uploading them |

From 2026.10.6 the reporter ends each run with a `Mergify CI` summary (2026.10.5
and earlier print nothing when the upload works). Check:
`MERGIFY_CI_ENABLE=true MERGIFY_CI_DEBUG=true npx vitest run` prints the
spans.
