# Playwright: `@mergifyio/playwright`

## Requirements

- **Node.js 22 or later** in the job that runs the tests (npm and pnpm only
  warn on an older one, Yarn 1 refuses). If the job runs an older Node.js, ask
  the user before changing it, and set up everything else first. Playwright's
  official container images ship a recent Node.js.
- `@playwright/test` 1.56 or later.

## Install

`npm install --save-dev @mergifyio/playwright` (or pnpm, Yarn, Bun), with the
package manager version the project pins, in the package that runs Playwright.

## Configure: two steps, both required

The first uploads the results, the second applies quarantine.

**1. Wrap the configuration** with `withMergify`. It keeps the existing
reporters, `globalSetup` and `globalTeardown`.

```typescript
import { defineConfig } from '@playwright/test';
import { withMergify } from '@mergifyio/playwright';

export default withMergify(
  defineConfig({
    // ... existing configuration
  })
);
```

CommonJS (`playwright.config.js` with `require`):

```javascript
const { defineConfig } = require('@playwright/test');
const { withMergify } = require('@mergifyio/playwright');

module.exports = withMergify(defineConfig({ /* ... */ }));
```

**2. Switch the imports** in every test file, and in every fixture file that
extends `test`, from `@playwright/test` to `@mergifyio/playwright`:

```typescript
import { test, expect } from '@mergifyio/playwright';
```

Type-only imports (`type Page`) can stay on `@playwright/test`. Without this
step, results upload but quarantined tests still fail the run.

## CI

- GitHub Actions: `MERGIFY_TOKEN: ${{ secrets.MERGIFY_TOKEN }}` on the step
  that runs Playwright.
- Matrix: `MERGIFY_TEST_JOB_NAME` per environment leg. `--shard` legs share one
  name: never put the shard index in it.
- **Several projects** (browsers) in one config share a test identity by
  default, so one project's failure is filed against the others. On a new
  setup, set `MERGIFY_PLAYWRIGHT_INCLUDE_PROJECT_IN_TEST_NAME: "true"` on the
  test step to keep each project's tests separate. On a repository that already
  reports to Mergify, it renames every test: set it only if the user asks.
- Tests in a Docker Compose service: load `containers.md`.
- No `continue-on-error`: quarantine is applied by the `test` import.

## Variables

| Variable | Purpose |
|---|---|
| `MERGIFY_TOKEN` | the `ci` application key |
| `MERGIFY_TEST_JOB_NAME` | the job name to report under |
| `MERGIFY_CI_ENABLE` | report even when `CI` is not set (was `PLAYWRIGHT_MERGIFY_ENABLE`) |
| `MERGIFY_PLAYWRIGHT_INCLUDE_PROJECT_IN_TEST_NAME` | prefix test names with the project (was `PLAYWRIGHT_MERGIFY_INCLUDE_PROJECT_IN_TEST_NAME`) |
| `MERGIFY_CI_DEBUG` | print the spans instead of uploading them |

Check: `MERGIFY_CI_ENABLE=true MERGIFY_CI_DEBUG=true npx playwright test <one spec file>`
prints the spans. Do not pass `--reporter`: it replaces the configured reporters.
Delete the `test-results/` and report folders the run leaves before committing.
