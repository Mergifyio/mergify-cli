//! Test-only helpers shared between `detector` and `scopes_send`.
//!
//! Both modules test CI-provider-aware code paths and need to scrub
//! the host's CI env vars before running each case — otherwise a
//! test running on a real Buildkite/Actions/Circle/Jenkins host
//! inherits provider state and the detector picks the wrong branch.
//! Two flavors: a sync `with_ci_env` and an async `with_ci_env_async`
//! (used by the `#[tokio::test]` cases in `scopes_send`).

use std::future::Future;

/// Env vars the CI-provider detection chain inspects. Clear every
/// one of them before applying the test-specific overrides, so the
/// host environment can't leak into the test.
///
/// `GITHUB_OUTPUT` belongs on this list too — when the suite runs
/// *on* a GHA runner that var points at the runner's real
/// step-output file, and any test that exercises a code path
/// appending a heredoc (e.g. `ci scopes` →
/// `MERGIFY_SCOPES<<ghadelimiter_…`) will splice its own
/// delimiter into the runner's file. GHA then fails the step with
/// "Matching delimiter not found". Scrubbing it forces the no-op
/// `env::var("GITHUB_OUTPUT").ok()` branch unless the test
/// explicitly points it at a temp file.
const CI_ENV_VARS: &[&str] = &[
    "JENKINS_URL",
    "GITHUB_ACTIONS",
    "GITHUB_REPOSITORY",
    "GITHUB_EVENT_PATH",
    "GITHUB_OUTPUT",
    "CIRCLECI",
    "CIRCLE_REPOSITORY_URL",
    "BUILDKITE",
    "BUILDKITE_REPO",
    "BUILDKITE_PULL_REQUEST",
    "GIT_URL",
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
