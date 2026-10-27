# Jest: `@mergifyio/jest`

## Requirements

- **Node.js 22 or later** in the job that runs the tests. If the job runs an
  older Node.js, ask the user before changing it, and set up everything else
  first. If they keep the older Node.js, use the JUnit upload instead
  (`junit.md`).
- Jest 29 or later.

## Install

`npm install --save-dev @mergifyio/jest`, `pnpm add -D @mergifyio/jest` or
`yarn add -D @mergifyio/jest`, with the version of the package manager the
project pins, in the package that runs Jest.

## Configure

Wrap the Jest configuration with `withMergify`, the only supported setup:

```js
// jest.config.js
const { withMergify } = require('@mergifyio/jest');

module.exports = withMergify({
  // ... existing configuration
});
```

```ts
// jest.config.ts
import type { Config } from 'jest';
import { withMergify } from '@mergifyio/jest';

const config: Config = {
  // ... existing configuration
};

export default withMergify(config);
```

- `withMergify` adds the Mergify reporter after the configured reporters (or
  after Jest's default one) and, where Jest's default test runner
  (jest-circus) runs, Mergify's test runner, which applies the quarantine: no
  `continue-on-error`.
- **A `testRunner` other than jest-circus** (`jest-jasmine2`, a custom one),
  in the configuration or in a project, is kept: its tests report, but
  quarantine, flaky detection and test retry do not apply to them. Do not
  change the runner; say so in the pull request and in your final answer.
- **A configuration in `package.json` or `jest.config.json`** has no function
  to call: move it to a `jest.config.js` (or `.ts`, `.mjs`, `.cjs`) and wrap
  it there.
- **`projects` listed by path** load their own files: wrap each of them too.
  Projects listed inline are covered by the root `withMergify`.
- **`--reporters` on the command line** replaces the configured reporters, the
  Mergify one included. Move each reporter it names, in order, into the
  configuration's `reporters`, then drop the flags from the test command:
  `--reporters=default --reporters=jest-junit` becomes
  `reporters: ['default', 'jest-junit']`. Dropping them without moving them
  stops what they produce, such as a JUnit file another step reads.
- Tests are named `<file> > <describe> > <title>`. A repository that already
  uploaded `jest-junit` reports to Mergify starts each test's history again
  under the new names: say so in the pull request.

## CI

- GitHub Actions: `MERGIFY_TOKEN: ${{ secrets.MERGIFY_TOKEN }}` on the step
  that runs Jest.
- Matrix: `MERGIFY_TEST_JOB_NAME` per leg. `--shard` legs share one name.

## Variables

| Variable | Purpose |
|---|---|
| `MERGIFY_TOKEN` | the `ci` application key |
| `MERGIFY_TEST_JOB_NAME` | the job name to report under |
| `MERGIFY_CI_ENABLE` | report even when `CI` is not set |
| `MERGIFY_CI_DEBUG` | print the spans instead of uploading them |

Check: `MERGIFY_CI_ENABLE=true MERGIFY_CI_DEBUG=true npx jest` prints the spans
and a `Mergify CI` summary.
