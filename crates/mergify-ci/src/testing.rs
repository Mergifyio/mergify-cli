//! Test-only helpers shared across the CI-aware command modules
//! (`detector`, `scopes_send`, `tests_show`, `tests_quarantine`).
//!
//! These modules test CI-provider-aware code paths and need to scrub
//! the host's CI env vars before running each case — otherwise a
//! test running on a real Buildkite/Actions/Circle/Jenkins host
//! inherits provider state and the detector picks the wrong branch.
//! Two flavors: a sync `with_ci_env` and an async `with_ci_env_async`
//! (used by the `#[tokio::test]` cases).

use std::future::Future;

/// Env vars the CI-provider detection chain inspects. Clear every
/// one of them before applying the test-specific overrides, so the
/// host environment can't leak into the test — running the test
/// suite *on* a real GitHub Actions / `CircleCI` / Jenkins / Buildkite
/// host would otherwise produce `vcs.ref.head.name` etc. values
/// taken from the runner instead of the test's explicit override
/// and silently fail.
///
/// `GITHUB_OUTPUT` belongs on this list too — when the suite runs
/// on a GHA runner that var points at the runner's real
/// step-output file, and any test that exercises a code path
/// appending a heredoc (e.g. `ci scopes` →
/// `MERGIFY_SCOPES<<ghadelimiter_…`) will splice its own
/// delimiter into the runner's file. GHA then fails the step with
/// "Matching delimiter not found". Scrubbing it forces the no-op
/// `env::var("GITHUB_OUTPUT").ok()` branch unless the test
/// explicitly points it at a temp file.
///
/// Keep this list aligned with every `env::var(...)` call across
/// `detector::*`; new detector helpers must add their inputs here
/// or their tests will be flaky on CI.
const CI_ENV_VARS: &[&str] = &[
    // Provider selection.
    "JENKINS_URL",
    "GITHUB_ACTIONS",
    "CIRCLECI",
    "BUILDKITE",
    // Repository identity (cross-provider).
    "GITHUB_REPOSITORY",
    "GITHUB_EVENT_PATH",
    "GITHUB_OUTPUT",
    "CIRCLE_REPOSITORY_URL",
    "BUILDKITE_REPO",
    "BUILDKITE_PULL_REQUEST",
    "GIT_URL",
    // GitHub Actions resource attributes.
    "GITHUB_EVENT_NAME",
    "GITHUB_HEAD_REF",
    "GITHUB_REF_NAME",
    "GITHUB_BASE_REF",
    "GITHUB_WORKFLOW",
    "GITHUB_JOB",
    "GITHUB_RUN_ID",
    "GITHUB_RUN_ATTEMPT",
    "GITHUB_SHA",
    "RUNNER_NAME",
    // CircleCI resource attributes.
    "CIRCLE_BRANCH",
    "CIRCLE_JOB",
    "CIRCLE_WORKFLOW_ID",
    "CIRCLE_BUILD_NUM",
    "CIRCLE_SHA1",
    "CIRCLE_PULL_REQUESTS",
    // Jenkins resource attributes.
    "JOB_NAME",
    "GIT_BRANCH",
    "GIT_COMMIT",
    "CHANGE_TARGET",
    "NODE_NAME",
    "BUILD_ID",
    // Buildkite resource attributes.
    "BUILDKITE_PIPELINE_SLUG",
    "BUILDKITE_LABEL",
    "BUILDKITE_STEP_KEY",
    "BUILDKITE_BRANCH",
    "BUILDKITE_PULL_REQUEST_BASE_BRANCH",
    "BUILDKITE_AGENT_NAME",
    "BUILDKITE_BUILD_ID",
    "BUILDKITE_BUILD_URL",
    "BUILDKITE_RETRY_COUNT",
    "BUILDKITE_COMMIT",
    // CLI-side metadata that the upload layer reads into resource
    // attributes; explicitly scrubbed so tests that don't set it
    // get a deterministic `mergify.test.job.name` (absent).
    "MERGIFY_TEST_JOB_NAME",
    // Consumed by `junit_process::command::resolve_test_exit_code`
    // (silent-failure detection). Scrub so the orchestrator tests
    // don't pick up a developer's local export or a CI host that
    // happens to set it, which would change which verdict branch
    // the assertions land in.
    "MERGIFY_TEST_EXIT_CODE",
];

fn merged_overrides(extra: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
    let mut vars: Vec<(String, Option<String>)> = CI_ENV_VARS
        .iter()
        .map(|k| ((*k).to_string(), None))
        .collect();
    for (k, v) in extra {
        vars.push(((*k).to_string(), v.map(ToString::to_string)));
    }
    vars
}

/// Run `f` with the CI-provider env vars cleared, plus the
/// `extra` overrides applied on top.
pub(crate) fn with_ci_env<F, R>(extra: &[(&str, Option<&str>)], f: F) -> R
where
    F: FnOnce() -> R,
{
    temp_env::with_vars(merged_overrides(extra), f)
}

/// Async counterpart to [`with_ci_env`]. Used by `#[tokio::test]`
/// cases in `scopes_send` — the sync variant can't bridge `.await`
/// points.
pub(crate) async fn with_ci_env_async<F, R>(extra: &[(&str, Option<&str>)], f: F) -> R
where
    F: Future<Output = R>,
{
    temp_env::async_with_vars(merged_overrides(extra), f).await
}

/// A high-entropy alphanumeric string of `len` chars, seeded so
/// different callers produce different content. Used by the
/// `junit_process::split` tests to fill stack traces with data gzip
/// can't collapse — a repeating or low-entropy filler would compress
/// to almost nothing and never force a split, so the compressed-size
/// path would go untested. Alphanumeric output is XML-safe, so the
/// same helper works both for building `TestCase` values directly and
/// for embedding inside a `<failure>` body in an XML fixture.
pub(crate) fn incompressible(seed: u64, len: usize) -> String {
    // xorshift64 → one alphanumeric char per step.
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            // `state % ALPHABET.len()` is always < 62, so it fits
            // usize on every target and the conversion can't fail;
            // `unwrap_or(0)` keeps it panic-free and ties the range to
            // the alphabet (no magic number).
            let idx = usize::try_from(state % ALPHABET.len() as u64).unwrap_or(0);
            char::from(ALPHABET[idx])
        })
        .collect()
}
