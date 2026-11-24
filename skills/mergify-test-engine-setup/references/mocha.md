# Mocha: `@mergifyio/mocha`

## Requirements

- **Node.js 22 or later** in the job that runs the tests. If the job runs an
  older Node.js, ask the user before changing it, and set up everything else
  first. If they keep the older Node.js, use the JUnit upload instead
  (`junit.md`).
- Mocha 8.2 or later: older Mocha has no global fixtures, which the plugin
  runs in. If the project pins an older Mocha, ask the user before upgrading
  it; if they keep it, use the JUnit upload instead (`junit.md`). Cypress is
  not Mocha's command line: it stays on the JUnit upload (`junit.md`).
- With `--parallel`, Mocha 8.3 or later: Mocha 8.2 runs serially only, its
  own parallel mode crashes in its worker pool (workerpool 6.0.2) on current
  Node.js, with or without the plugin. If the project runs Mocha 8.2 with
  `--parallel` (or `parallel: true` in its configuration), ask the user
  before upgrading Mocha or making the run serial.
- On Node.js 26, Mocha 10.6 or later: Mocha 8.3 to 10.5 crash on start there,
  with or without the plugin (they pin yargs 16.2.0, which Node.js 26 loads
  as an ES module). If the job runs Node.js 26 with an older Mocha, ask the
  user before upgrading Mocha.

## Install

`npm install --save-dev @mergifyio/mocha`, `pnpm add -D @mergifyio/mocha` or
`yarn add -D @mergifyio/mocha`, with the version of the package manager the
project pins, in the package that runs Mocha.

## Configure

Have Mocha require the plugin, in its configuration file:

```js
// .mocharc.js
module.exports = {
  require: ['@mergifyio/mocha'],
};
```

```yaml
# .mocharc.yml
require:
  - '@mergifyio/mocha'
```

or with `--require @mergifyio/mocha` on the command line. Mocha merges the
`require` lists of its configuration and its command line.

- The plugin takes no reporter: keep `spec`, `mocha-junit-reporter` or any
  other. It applies the quarantine itself: no `continue-on-error`.
- It works in Mocha's main process and in its workers (`--parallel`, Mocha
  8.3 or later).
- Code that runs Mocha through its API, not its command line, does not read
  `--require`: pass the plugin's fixtures as the plugin's README shows
  (`mergify()` returns `mochaGlobalSetup` and `mochaGlobalTeardown`).
- Tests are named `<file> > <describe> > <title>`. A repository that already
  uploaded JUnit reports to Mergify starts each test's history again under the
  new names: say so in the pull request.

## CI

- GitHub Actions: `MERGIFY_TOKEN: ${{ secrets.MERGIFY_TOKEN }}` on the step
  that runs Mocha.
- Matrix: `MERGIFY_TEST_JOB_NAME` per leg; a job that splits its files between
  several Mocha runs gives each run its own name.

## Variables

| Variable | Purpose |
|---|---|
| `MERGIFY_TOKEN` | the `ci` application key |
| `MERGIFY_TEST_JOB_NAME` | the job name to report under |
| `MERGIFY_CI_ENABLE` | report even when `CI` is not set |
| `MERGIFY_CI_DEBUG` | print the spans instead of uploading them |

Check: `MERGIFY_CI_ENABLE=true MERGIFY_CI_DEBUG=true npx mocha` prints the
spans and a `Mergify CI` summary.
