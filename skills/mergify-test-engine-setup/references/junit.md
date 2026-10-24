# Frameworks without a plugin: JUnit report + upload

1. **Write a JUnit XML report from the existing test run**: the framework's
   JUnit reporter or a converter, configured in its configuration file or with
   reporter options. Do not add a second test command. Keep the framework's
   normal console output.
2. **Upload it in the same job**: on GitHub Actions with the
   `mergifyio/gha-mergify-ci` action (`github-actions.md`); on CircleCI and
   Jenkins with `mergify ci junit-process` (`circleci.md`, `jenkins.md`); on
   Buildkite with the `mergifyio/mergify-ci` plugin (`buildkite.md`).
3. **Ignore the report file** in `.gitignore` if it lands in the working tree.

## Jest

Add `jest-junit` as a dev dependency and register it next to the default
reporter, where the Jest configuration lives (`jest.config.*` or `package.json`):

```javascript
reporters: [
  'default',
  ['jest-junit', {
    outputDirectory: '.',
    outputName: 'junit.xml',
    // Without it, a file with no top-level `describe` reports its suite as
    // "undefined". On a project already reporting, it renames the suites.
    suiteNameTemplate: '{filepath}',
  }],
],
```

Or keep the configuration and pass
`--reporters=default --reporters=jest-junit` with `JEST_JUNIT_OUTPUT_DIR` and
`JEST_JUNIT_OUTPUT_NAME`. `jest-junit` 17 needs Node.js 20+; use 16 on older
Node.js. (`--outputFile` does not apply to `jest-junit`.)

## Go

`go test` has no JUnit output: convert its JSON output with `go-junit-report`
v2 (module `github.com/jstemmer/go-junit-report/v2`; the path without `/v2` is
v1, which has no `-parser` flag):

```bash
set -o pipefail
go test -json ./... | go-junit-report -parser gojson > junit.xml
```

- **Keep the test exit code**: a pipe reports the last command's code unless
  `pipefail` is on. GitHub Actions' default `run` shell does not set it: add
  `set -o pipefail` or `shell: bash`.
- **Do not merge stderr into the report** (`2>&1`): compiler and linker
  warnings (macOS `ld: warning` lines with Go before 1.21) become failing
  "Build error" tests and fail the upload step although every test passed.
- **Feed it `go test`'s own output**: wrappers such as `richgo` produce output
  the converters cannot parse. Keep the wrapper for the console if the project
  wants it (`tee`), and convert the raw output.
- **Pin a converter version the oldest Go in the matrix can install**:
  `go install github.com/jstemmer/go-junit-report/v2@v2.1.0` works from Go
  1.16; recent `gotestsum` needs a recent Go (v1.13.0 requires Go 1.24).
- Keep the repository's `go test` flags (`-race`, `-ldflags`, build tags).

## JUnit (Java)

- Maven Surefire writes `target/surefire-reports/*.xml` by default.
- Gradle's `test` task writes `build/test-results/test/*.xml` by default.

## Cypress and Mocha

Use `cypress-multi-reporters` (Cypress) or `mocha-multi-reporters` (Mocha) with
`mocha-junit-reporter`, keeping the `spec` reporter for the console:

```json
{
  "reporterEnabled": "spec, mocha-junit-reporter",
  "mochaJunitReporterReporterOptions": {
    "mochaFile": "results/junit-[hash].xml"
  }
}
```

Upload with a glob (`results/junit-*.xml`).

## Others

Use the framework's own JUnit option:

- Cucumber: `--format junit --out <dir>`.
- minitest: the `minitest-junit` gem (`require 'minitest/junit'`) or
  `minitest-reporters`' JUnit reporter.
- PHPUnit and Pest: `--log-junit junit.xml`, or `<junit outputFile="junit.xml"/>`
  in the configuration's `<logging>`.
- NUnit and MSTest: the JUnit test logger,
  `dotnet test --logger "junit;LogFilePath=junit.xml"`.
- TestNG: its built-in `JUnitReportReporter` listener.
- Rust: `cargo-nextest`, which writes JUnit XML with a `[profile.<name>.junit]`
  section in `.config/nextest.toml`.
