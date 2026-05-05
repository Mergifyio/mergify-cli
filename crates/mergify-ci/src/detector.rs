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
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    let valid = |s: &str| {
        s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '-')
    };
    if !valid(owner) || !valid(repo) {
        return None;
    }
    Some(format!("{owner}/{repo}"))
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
    let Ok(event_path) = env::var("GITHUB_EVENT_PATH") else {
        return Ok(None);
    };
    if event_path.is_empty() {
        return Ok(None);
    }
    // A missing event file means "this isn't a GitHub Actions
    // pull-request event" — match the Python CLI and treat it as
    // "no PR detected", not a Configuration error.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Clear every CI-provider env var the detector inspects, then
    /// apply the test-specific overrides on top. Without this, a
    /// test running on a real CI host inherits provider state and
    /// the detector picks the wrong branch.
    pub(crate) fn with_ci_env<F: FnOnce() -> R, R>(extra: &[(&str, Option<&str>)], f: F) -> R {
        let mut vars: Vec<(String, Option<String>)> = [
            "JENKINS_URL",
            "GITHUB_ACTIONS",
            "GITHUB_REPOSITORY",
            "GITHUB_EVENT_PATH",
            "CIRCLECI",
            "CIRCLE_REPOSITORY_URL",
            "BUILDKITE",
            "BUILDKITE_REPO",
            "BUILDKITE_PULL_REQUEST",
            "GIT_URL",
        ]
        .into_iter()
        .map(|k| (k.to_string(), None))
        .collect();
        for (k, v) in extra {
            vars.push((k.to_string(), v.map(ToString::to_string)));
        }
        temp_env::with_vars(vars, f)
    }

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
}
