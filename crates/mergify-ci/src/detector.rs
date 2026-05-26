//! CI environment detection — Rust mirror of
//! `mergify_cli/ci/detector.py`.
//!
//! Every public item here corresponds to a Python function or
//! constant of the same name. Only the items consumed by ported
//! Rust commands are mirrored; the rest stays in Python until its
//! command is ported.

use std::env;

use mergify_core::CliError;
use url::Url;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CIProvider {
    GithubActions,
    CircleCi,
    Jenkins,
    Buildkite,
}

impl CIProvider {
    /// String identifier Python emits as the `cicd.provider.name`
    /// span attribute. Must match `mergify_cli.ci.detector.CIProviderT`
    /// (`snake_case`, no underscore for the multi-word ones except
    /// `github_actions`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GithubActions => "github_actions",
            Self::CircleCi => "circleci",
            Self::Jenkins => "jenkins",
            Self::Buildkite => "buildkite",
        }
    }
}

#[must_use]
pub fn get_ci_provider() -> Option<CIProvider> {
    if env::var("JENKINS_URL").ok().is_some_and(|v| !v.is_empty()) {
        return Some(CIProvider::Jenkins);
    }
    if env::var("GITHUB_ACTIONS").as_deref() == Ok("true") {
        return Some(CIProvider::GithubActions);
    }
    if env::var("CIRCLECI").as_deref() == Ok("true") {
        return Some(CIProvider::CircleCi);
    }
    if env::var("BUILDKITE").as_deref() == Ok("true") {
        return Some(CIProvider::Buildkite);
    }
    None
}

/// Mirror of Python's private ``_get_github_repository_from_env``.
/// Reads ``env_name`` from the process environment and parses the
/// repository URL into ``owner/repo``. Returns ``None`` when the var
/// is unset or the value doesn't parse.
fn get_github_repository_from_env(env_name: &str) -> Option<String> {
    let raw = env::var(env_name).ok()?;
    parse_repository_url(&raw)
}

fn parse_repository_url(url_str: &str) -> Option<String> {
    let url_str = url_str.trim();
    if url_str.is_empty() {
        return None;
    }

    if let Some(rest) = url_str.strip_prefix("git@") {
        let (_host, path) = rest.split_once(':')?;
        return validate_owner_repo(path.trim_end_matches('/').trim_end_matches(".git"));
    }

    if url_str.starts_with("http://") || url_str.starts_with("https://") {
        let parsed = Url::parse(url_str).ok()?;
        // Python's regex anchors to end-of-string, so a URL carrying
        // a query or fragment never matches. Reject them here too,
        // otherwise `https://github.com/owner/repo?tab=readme` would
        // parse to `owner/repo` in Rust but be ignored by Python.
        if parsed.query().is_some() || parsed.fragment().is_some() {
            return None;
        }
        let path = parsed
            .path()
            .trim_start_matches('/')
            .trim_end_matches('/')
            .trim_end_matches(".git");
        return validate_owner_repo(path);
    }

    validate_owner_repo(url_str.trim_end_matches('/').trim_end_matches(".git"))
}

fn validate_owner_repo(path: &str) -> Option<String> {
    let (owner, repo) = path.split_once('/')?;
    if !is_valid_segment(owner) || !is_valid_segment(repo) || repo.contains('/') {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

/// Allowed character set for an `owner` or `repo` path segment.
///
/// Matches GitHub's allowance (alphanumerics, `_`, `.`, `-`) and the
/// regex used by `parse_repository_url`. Rejects every URL-reserved
/// character (`?`, `#`, `%`, `/`, space) so callers can interpolate
/// the segments straight into a request path without percent-encoding
/// and without enabling path or query injection.
fn is_valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '-')
}

/// Clap `value_parser` for `--repository`. Returning `Result<_, String>`
/// makes clap surface a bad value as exit code 2 instead of letting it
/// slip through to runtime as a `Configuration` error.
///
/// # Errors
///
/// Returns the validation message from `split_owner_repo` when the
/// input is not exactly `owner/repo` with allowed characters.
pub fn parse_owner_repo(value: &str) -> Result<String, String> {
    split_owner_repo(value)
        .map(|_| value.to_string())
        .map_err(|e| e.to_string())
}

/// Split a `"owner/repo"` string into its two parts. The
/// Mergify CI Insights endpoints take owner and repository name as
/// separate path segments, while `--repository` accepts the
/// `owner/repo` shorthand. Rejects empty parts and any character
/// outside `is_valid_segment` so the values can be interpolated into
/// URL paths without further escaping.
pub fn split_owner_repo(value: &str) -> Result<(&str, &str), CliError> {
    let mismatch = || {
        CliError::Configuration(format!(
            "invalid repository {value:?}: expected `owner/repo`",
        ))
    };
    let (owner, repo) = value.split_once('/').ok_or_else(mismatch)?;
    if !is_valid_segment(owner) || !is_valid_segment(repo) || repo.contains('/') {
        return Err(mismatch());
    }
    Ok((owner, repo))
}

#[must_use]
pub fn get_github_repository() -> Option<String> {
    match get_ci_provider()? {
        CIProvider::GithubActions => env::var("GITHUB_REPOSITORY").ok().filter(|s| !s.is_empty()),
        CIProvider::CircleCi => get_github_repository_from_env("CIRCLE_REPOSITORY_URL"),
        CIProvider::Jenkins => get_github_repository_from_env("GIT_URL"),
        CIProvider::Buildkite => get_github_repository_from_env("BUILDKITE_REPO"),
    }
}

pub fn get_github_pull_request_number() -> Result<Option<u64>, CliError> {
    match get_ci_provider() {
        Some(CIProvider::GithubActions) => read_github_event_pull_request_number(),
        Some(CIProvider::Buildkite) => match env::var("BUILDKITE_PULL_REQUEST") {
            Ok(pr) if !pr.is_empty() && pr != "false" => pr.parse::<u64>().map(Some).map_err(|e| {
                CliError::Configuration(format!("BUILDKITE_PULL_REQUEST is not an integer: {e}"))
            }),
            _ => Ok(None),
        },
        _ => Ok(None),
    }
}

fn read_github_event_pull_request_number() -> Result<Option<u64>, CliError> {
    // The PR-number lookup is strict about JSON failures because
    // it's the only signal that decides whether `scopes-send` runs
    // at all — silently swallowing a parse error there would hide
    // a misconfigured workflow. The head-SHA lookup (see
    // [`read_github_event_pull_request_head_sha`]) has a sane
    // fallback (`GITHUB_SHA`), so it stays lenient.
    let Some(event_path) = env::var("GITHUB_EVENT_PATH").ok().filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let content = match std::fs::read_to_string(&event_path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(CliError::Configuration(format!(
                "cannot read GITHUB_EVENT_PATH ({event_path}): {e}"
            )));
        }
    };
    let event: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        CliError::Configuration(format!("GITHUB_EVENT_PATH is not valid JSON: {e}"))
    })?;
    Ok(event
        .pointer("/pull_request/number")
        .and_then(serde_json::Value::as_u64))
}

/// `cicd.pipeline.name` resource attribute. None when the
/// provider can't be detected or its env var isn't set.
#[must_use]
pub fn get_pipeline_name() -> Option<String> {
    let var = match get_ci_provider()? {
        CIProvider::GithubActions => "GITHUB_WORKFLOW",
        CIProvider::Jenkins => "JOB_NAME",
        CIProvider::Buildkite => "BUILDKITE_PIPELINE_SLUG",
        CIProvider::CircleCi => return None,
    };
    non_empty_env(var)
}

/// `cicd.pipeline.task.name` — the job within a pipeline.
#[must_use]
pub fn get_job_name() -> Option<String> {
    match get_ci_provider()? {
        CIProvider::GithubActions => non_empty_env("GITHUB_JOB"),
        CIProvider::CircleCi => non_empty_env("CIRCLE_JOB"),
        CIProvider::Jenkins => non_empty_env("JOB_NAME"),
        CIProvider::Buildkite => {
            non_empty_env("BUILDKITE_LABEL").or_else(|| non_empty_env("BUILDKITE_STEP_KEY"))
        }
    }
}

/// `vcs.ref.head.name` — name of the branch the test ran on.
#[must_use]
pub fn get_head_ref_name() -> Option<String> {
    match get_ci_provider()? {
        CIProvider::GithubActions => {
            // GitHub Actions sets `GITHUB_HEAD_REF` only on PR
            // events. Fall back to `GITHUB_REF_NAME` everywhere
            // else (the bare branch name, not `<pr#>/merge`).
            non_empty_env("GITHUB_HEAD_REF").or_else(|| non_empty_env("GITHUB_REF_NAME"))
        }
        CIProvider::CircleCi => non_empty_env("CIRCLE_BRANCH"),
        CIProvider::Jenkins => non_empty_env("GIT_BRANCH").map(|raw| {
            // Jenkins' Git plugin sets `GIT_BRANCH` to
            // `<remote>/<branch>` (or `refs/heads/<branch>` when
            // the job's configured for a refspec). Strip the
            // common prefixes so the wire value matches what
            // GitHub Actions reports.
            for prefix in ["origin/", "refs/heads/"] {
                if let Some(stripped) = raw.strip_prefix(prefix) {
                    return stripped.to_string();
                }
            }
            raw
        }),
        CIProvider::Buildkite => non_empty_env("BUILDKITE_BRANCH"),
    }
}

/// `vcs.ref.base.name` — PR target branch, when running for a PR.
#[must_use]
pub fn get_base_ref_name() -> Option<String> {
    match get_ci_provider()? {
        CIProvider::GithubActions => non_empty_env("GITHUB_BASE_REF"),
        CIProvider::Jenkins => non_empty_env("CHANGE_TARGET"),
        CIProvider::Buildkite => non_empty_env("BUILDKITE_PULL_REQUEST_BASE_BRANCH"),
        CIProvider::CircleCi => None,
    }
}

/// `cicd.pipeline.runner.name` — host / agent identity.
#[must_use]
pub fn get_cicd_pipeline_runner_name() -> Option<String> {
    match get_ci_provider()? {
        CIProvider::GithubActions => non_empty_env("RUNNER_NAME"),
        CIProvider::Jenkins => non_empty_env("NODE_NAME"),
        CIProvider::Buildkite => non_empty_env("BUILDKITE_AGENT_NAME"),
        CIProvider::CircleCi => None,
    }
}

/// `cicd.pipeline.run.id` — the workflow / build identifier.
/// Returned as a string because GitHub uses an integer-like ID
/// while Jenkins and Buildkite emit free-form strings.
#[must_use]
pub fn get_cicd_pipeline_run_id() -> Option<String> {
    match get_ci_provider()? {
        CIProvider::GithubActions => non_empty_env("GITHUB_RUN_ID"),
        CIProvider::CircleCi => non_empty_env("CIRCLE_WORKFLOW_ID"),
        CIProvider::Jenkins => non_empty_env("BUILD_ID"),
        CIProvider::Buildkite => non_empty_env("BUILDKITE_BUILD_ID"),
    }
}

/// `cicd.pipeline.run.attempt` — 1-indexed retry counter.
#[must_use]
pub fn get_cicd_pipeline_run_attempt() -> Option<u64> {
    match get_ci_provider()? {
        CIProvider::GithubActions => non_empty_env("GITHUB_RUN_ATTEMPT")?.parse().ok(),
        CIProvider::CircleCi => non_empty_env("CIRCLE_BUILD_NUM")?.parse().ok(),
        // Buildkite uses 0-indexed retries; add 1 so a fresh run
        // reads as attempt 1 (matching the GHA/CircleCI semantics).
        CIProvider::Buildkite => non_empty_env("BUILDKITE_RETRY_COUNT")?
            .parse::<u64>()
            .ok()
            .map(|n| n + 1),
        CIProvider::Jenkins => None,
    }
}

/// `cicd.pipeline.run.url` — direct link to the running build.
#[must_use]
pub fn get_cicd_pipeline_run_url() -> Option<String> {
    match get_ci_provider()? {
        CIProvider::Buildkite => non_empty_env("BUILDKITE_BUILD_URL"),
        _ => None,
    }
}

/// `vcs.repository.url.full` — clone URL of the repository under
/// test. GitHub Actions has no equivalent env (the repo is implicit
/// from `GITHUB_REPOSITORY`); we report `None` there.
#[must_use]
pub fn get_repository_url() -> Option<String> {
    match get_ci_provider()? {
        CIProvider::Buildkite => non_empty_env("BUILDKITE_REPO"),
        CIProvider::CircleCi => non_empty_env("CIRCLE_REPOSITORY_URL"),
        CIProvider::Jenkins => non_empty_env("GIT_URL"),
        CIProvider::GithubActions => None,
    }
}

/// `vcs.ref.head.revision` — the commit SHA the tests ran against.
///
/// For GitHub Actions PR builds, `GITHUB_SHA` is the *synthetic
/// merge commit* GitHub creates by merging the PR head into the
/// base — not the actual code under test. The event payload at
/// `GITHUB_EVENT_PATH` carries the real `pull_request.head.sha`,
/// which is what dashboards correlate with the contributor's
/// commit. We prefer the event-payload value when present and
/// fall back to `GITHUB_SHA` otherwise.
///
/// For other providers we only have the bare env var today; the
/// `CircleCI` PR-build API fallback Python implements stays
/// Python-side until a Rust HTTP shim for GitHub's REST API lands.
#[must_use]
pub fn get_head_sha() -> Option<String> {
    match get_ci_provider()? {
        CIProvider::GithubActions => get_github_actions_head_sha(),
        CIProvider::CircleCi => non_empty_env("CIRCLE_SHA1"),
        CIProvider::Jenkins => non_empty_env("GIT_COMMIT"),
        CIProvider::Buildkite => non_empty_env("BUILDKITE_COMMIT"),
    }
}

fn get_github_actions_head_sha() -> Option<String> {
    if env::var("GITHUB_EVENT_NAME").as_deref() == Ok("pull_request") {
        if let Some(sha) = read_github_event_pull_request_head_sha() {
            return Some(sha);
        }
    }
    non_empty_env("GITHUB_SHA")
}

/// Read `GITHUB_EVENT_PATH` and pluck the
/// `pull_request.head.sha` out of the JSON. Returns `None` for
/// every "not applicable" case — env unset, file missing, file
/// not JSON, key not present — so the caller can quietly fall
/// back to `GITHUB_SHA` without surfacing an error to the user.
fn read_github_event_pull_request_head_sha() -> Option<String> {
    let event = read_github_event_json()?;
    event
        .pointer("/pull_request/head/sha")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn read_github_event_json() -> Option<serde_json::Value> {
    let event_path = env::var("GITHUB_EVENT_PATH").ok()?;
    if event_path.is_empty() {
        return None;
    }
    let content = std::fs::read_to_string(&event_path).ok()?;
    serde_json::from_str(&content).ok()
}

fn non_empty_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::with_ci_env;

    #[test]
    fn ci_provider_jenkins_takes_precedence() {
        with_ci_env(
            &[
                ("JENKINS_URL", Some("http://jenkins")),
                ("GITHUB_ACTIONS", Some("true")),
                ("CIRCLECI", Some("true")),
                ("BUILDKITE", Some("true")),
            ],
            || {
                assert_eq!(get_ci_provider(), Some(CIProvider::Jenkins));
            },
        );
    }

    #[test]
    fn ci_provider_returns_none_when_unset() {
        with_ci_env(&[], || {
            assert_eq!(get_ci_provider(), None);
        });
    }

    #[test]
    fn github_repository_github_actions() {
        with_ci_env(
            &[
                ("GITHUB_ACTIONS", Some("true")),
                ("GITHUB_REPOSITORY", Some("owner/repo")),
            ],
            || {
                assert_eq!(get_github_repository().as_deref(), Some("owner/repo"));
            },
        );
    }

    #[test]
    fn github_repository_buildkite_ssh() {
        with_ci_env(
            &[
                ("BUILDKITE", Some("true")),
                ("BUILDKITE_REPO", Some("git@github.com:owner/repo.git")),
            ],
            || {
                assert_eq!(get_github_repository().as_deref(), Some("owner/repo"));
            },
        );
    }

    #[test]
    fn github_repository_buildkite_https() {
        with_ci_env(
            &[
                ("BUILDKITE", Some("true")),
                ("BUILDKITE_REPO", Some("https://github.com/owner/repo")),
            ],
            || {
                assert_eq!(get_github_repository().as_deref(), Some("owner/repo"));
            },
        );
    }

    #[test]
    fn github_repository_circleci() {
        with_ci_env(
            &[
                ("CIRCLECI", Some("true")),
                (
                    "CIRCLE_REPOSITORY_URL",
                    Some("git@github.com:owner/repo.git"),
                ),
            ],
            || {
                assert_eq!(get_github_repository().as_deref(), Some("owner/repo"));
            },
        );
    }

    #[test]
    fn github_repository_jenkins() {
        with_ci_env(
            &[
                ("JENKINS_URL", Some("http://jenkins")),
                ("GIT_URL", Some("https://github.com/owner/repo.git")),
            ],
            || {
                assert_eq!(get_github_repository().as_deref(), Some("owner/repo"));
            },
        );
    }

    #[test]
    fn github_repository_returns_none_with_no_provider() {
        with_ci_env(&[("GITHUB_REPOSITORY", Some("owner/repo"))], || {
            assert_eq!(get_github_repository(), None);
        });
    }

    #[test]
    fn pull_request_buildkite_reads_env() {
        with_ci_env(
            &[
                ("BUILDKITE", Some("true")),
                ("BUILDKITE_PULL_REQUEST", Some("42")),
            ],
            || {
                assert_eq!(get_github_pull_request_number().unwrap(), Some(42));
            },
        );
    }

    #[test]
    fn pull_request_buildkite_returns_none_when_false() {
        with_ci_env(
            &[
                ("BUILDKITE", Some("true")),
                ("BUILDKITE_PULL_REQUEST", Some("false")),
            ],
            || {
                assert_eq!(get_github_pull_request_number().unwrap(), None);
            },
        );
    }

    #[test]
    fn pull_request_buildkite_returns_none_when_unset() {
        with_ci_env(&[("BUILDKITE", Some("true"))], || {
            assert_eq!(get_github_pull_request_number().unwrap(), None);
        });
    }

    #[test]
    fn pull_request_returns_none_with_no_provider() {
        with_ci_env(&[], || {
            assert_eq!(get_github_pull_request_number().unwrap(), None);
        });
    }

    #[test]
    fn pull_request_github_actions_reads_event_json() {
        let tmp = tempfile::tempdir().unwrap();
        let event_path = tmp.path().join("event.json");
        std::fs::write(
            &event_path,
            serde_json::json!({ "pull_request": { "number": 123 } }).to_string(),
        )
        .unwrap();
        with_ci_env(
            &[
                ("GITHUB_ACTIONS", Some("true")),
                ("GITHUB_EVENT_PATH", Some(event_path.to_str().unwrap())),
            ],
            || {
                assert_eq!(get_github_pull_request_number().unwrap(), Some(123));
            },
        );
    }

    #[test]
    fn pull_request_github_actions_missing_event_file_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope.json");
        with_ci_env(
            &[
                ("GITHUB_ACTIONS", Some("true")),
                ("GITHUB_EVENT_PATH", Some(missing.to_str().unwrap())),
            ],
            || {
                assert_eq!(get_github_pull_request_number().unwrap(), None);
            },
        );
    }

    #[test]
    fn split_owner_repo_accepts_owner_repo() {
        assert_eq!(
            split_owner_repo("Mergifyio/monorepo").unwrap(),
            ("Mergifyio", "monorepo")
        );
        assert_eq!(split_owner_repo("a/b").unwrap(), ("a", "b"));
    }

    #[test]
    fn split_owner_repo_rejects_inputs_without_exactly_one_slash() {
        for bad in ["", "owner", "owner/", "/repo", "a/b/c", "/", "//"] {
            let err = split_owner_repo(bad).unwrap_err();
            assert!(
                matches!(err, CliError::Configuration(_)),
                "input {bad:?} should map to Configuration, got {err:?}",
            );
            assert!(
                err.to_string().contains("owner/repo"),
                "error for {bad:?} should mention expected shape, got: {err}",
            );
        }
    }

    #[test]
    fn split_owner_repo_rejects_url_reserved_characters() {
        // These would otherwise inject extra path or query segments
        // when interpolated into a request URL.
        for bad in [
            "owner/repo?x=1",
            "owner/repo#frag",
            "owner/repo%2e",
            "own er/repo",
            "owner /repo",
            "owner/re po",
        ] {
            let err = split_owner_repo(bad).unwrap_err();
            assert!(
                matches!(err, CliError::Configuration(_)),
                "input {bad:?} should map to Configuration, got {err:?}",
            );
        }
    }

    #[test]
    fn parse_repository_url_handles_known_shapes() {
        let cases = [
            ("git@github.com:owner/repo.git", Some("owner/repo")),
            ("git@github.com:owner/repo", Some("owner/repo")),
            ("git@gitlab.example.com:owner/repo.git", Some("owner/repo")),
            ("https://github.com/owner/repo", Some("owner/repo")),
            ("https://github.com/owner/repo.git", Some("owner/repo")),
            ("https://github.com/owner/repo/", Some("owner/repo")),
            ("http://github.com:8080/owner/repo", Some("owner/repo")),
            ("owner/repo", Some("owner/repo")),
            ("https://github.com/owner/repo/sub", None),
            // Python's regex anchors at end-of-string, so URLs with
            // a query or fragment never match.
            ("https://github.com/owner/repo?tab=readme", None),
            ("https://github.com/owner/repo.git?ref=main", None),
            ("https://github.com/owner/repo#readme", None),
            ("not-a-url", None),
            ("", None),
        ];
        for (input, expected) in cases {
            assert_eq!(
                parse_repository_url(input).as_deref(),
                expected,
                "parse_repository_url({input:?}) mismatch"
            );
        }
    }

    #[test]
    fn head_sha_prefers_pr_event_payload_over_github_sha() {
        // PR events: `GITHUB_SHA` is the synthetic merge commit
        // GitHub creates by pre-merging the PR; the contributor's
        // actual head sha lives in the event payload at
        // `pull_request.head.sha`. Dashboards correlate with the
        // payload value, so we must prefer it.
        let tmp = tempfile::tempdir().unwrap();
        let event_path = tmp.path().join("event.json");
        std::fs::write(
            &event_path,
            serde_json::json!({
                "pull_request": {
                    "number": 7,
                    "head": { "sha": "feedface00000000000000000000000000000000" }
                }
            })
            .to_string(),
        )
        .unwrap();

        with_ci_env(
            &[
                ("GITHUB_ACTIONS", Some("true")),
                ("GITHUB_EVENT_NAME", Some("pull_request")),
                ("GITHUB_EVENT_PATH", Some(event_path.to_str().unwrap())),
                (
                    "GITHUB_SHA",
                    Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"),
                ),
            ],
            || {
                assert_eq!(
                    get_head_sha().as_deref(),
                    Some("feedface00000000000000000000000000000000"),
                );
            },
        );
    }

    #[test]
    fn head_sha_falls_back_to_github_sha_when_event_lacks_pr_head() {
        // push events still leave `GITHUB_EVENT_PATH` pointing at a
        // payload, but it has no `pull_request` field. Fall back to
        // `GITHUB_SHA` rather than returning None.
        let tmp = tempfile::tempdir().unwrap();
        let event_path = tmp.path().join("event.json");
        std::fs::write(&event_path, serde_json::json!({}).to_string()).unwrap();
        with_ci_env(
            &[
                ("GITHUB_ACTIONS", Some("true")),
                ("GITHUB_EVENT_NAME", Some("push")),
                ("GITHUB_EVENT_PATH", Some(event_path.to_str().unwrap())),
                ("GITHUB_SHA", Some("deadbeef")),
            ],
            || {
                assert_eq!(get_head_sha().as_deref(), Some("deadbeef"));
            },
        );
    }

    #[test]
    fn head_sha_uses_github_sha_when_event_path_missing() {
        // Workflows without an event file (e.g. local
        // `act` runs) still set GITHUB_SHA — we must not regress
        // to `None` just because the JSON file isn't there.
        with_ci_env(
            &[
                ("GITHUB_ACTIONS", Some("true")),
                ("GITHUB_EVENT_NAME", Some("pull_request")),
                ("GITHUB_EVENT_PATH", Some("/this/path/does/not/exist")),
                ("GITHUB_SHA", Some("cafef00d")),
            ],
            || {
                assert_eq!(get_head_sha().as_deref(), Some("cafef00d"));
            },
        );
    }
}
