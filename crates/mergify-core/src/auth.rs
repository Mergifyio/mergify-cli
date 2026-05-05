//! Resolve `--token`, `--api-url`, and `--repository` with the
//! same fallback order the Python CLI used.
//!
//! Token: `--token` flag → `MERGIFY_TOKEN` env → `GITHUB_TOKEN`
//! env → `gh auth token` (the GitHub CLI). Mirrors Python's
//! `utils.get_default_token`.
//!
//! Repository: `--repository` flag → `GITHUB_REPOSITORY` env →
//! `git config --get remote.origin.url` parsed into `<owner>/<repo>`.
//! Mirrors Python's `utils.get_default_repository` + `utils.get_slug`.
//!
//! API URL: `--api-url` flag → `MERGIFY_API_URL` env → default
//! `https://api.mergify.com`.
//!
//! Each ported command resolves these once before doing any
//! network or interactive work. The Rust copies that previously
//! lived in `mergify-config::simulate`, `mergify-ci::scopes_send`,
//! and `mergify-queue::auth` were missing the `gh auth token` and
//! `git config` fallbacks — that's why this module exists.

use std::env;
use std::process::Command;

use url::Url;

use crate::CliError;

const DEFAULT_API_URL: &str = "https://api.mergify.com";

/// Resolve the Mergify API bearer token.
///
/// Precedence: explicit `--token`, then `MERGIFY_TOKEN`, then
/// `GITHUB_TOKEN`, then the output of `gh auth token`. Errors when
/// none of those produce a non-empty value.
pub fn resolve_token(explicit: Option<&str>) -> Result<String, CliError> {
    if let Some(value) = explicit.filter(|s| !s.is_empty()) {
        return Ok(value.to_string());
    }
    for env_name in ["MERGIFY_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(value) = env::var(env_name) {
            if !value.is_empty() {
                return Ok(value);
            }
        }
    }
    if let Ok(token) = gh_auth_token() {
        if !token.is_empty() {
            return Ok(token);
        }
    }
    Err(CliError::Configuration(
        "please set the 'MERGIFY_TOKEN' or 'GITHUB_TOKEN' environment variable, \
         or make sure that the gh client is installed and you are authenticated"
            .to_string(),
    ))
}

/// Resolve the Mergify API base URL. Falls back to the
/// `MERGIFY_API_URL` env var, then the default
/// `https://api.mergify.com`.
pub fn resolve_api_url(explicit: Option<&str>) -> Result<Url, CliError> {
    let raw = explicit
        .map(str::to_string)
        .or_else(|| env::var("MERGIFY_API_URL").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_API_URL.to_string());
    Url::parse(&raw).map_err(|e| CliError::Configuration(format!("invalid --api-url {raw:?}: {e}")))
}

/// Resolve the repository (`<owner>/<repo>`).
///
/// Precedence: explicit `--repository`, then `GITHUB_REPOSITORY`
/// env, then `git config --get remote.origin.url` parsed via
/// [`parse_slug`]. Errors when none of those yield a slug.
pub fn resolve_repository(explicit: Option<&str>) -> Result<String, CliError> {
    if let Some(value) = explicit.filter(|s| !s.is_empty()) {
        return Ok(value.to_string());
    }
    if let Ok(value) = env::var("GITHUB_REPOSITORY") {
        if !value.is_empty() {
            return Ok(value);
        }
    }
    if let Some(remote) = git_remote_origin_url() {
        if let Some(slug) = parse_slug(&remote) {
            return Ok(slug);
        }
    }
    Err(CliError::Configuration(
        "--repository not provided, GITHUB_REPOSITORY env var is unset, and \
         the local git config has no usable `remote.origin.url`"
            .to_string(),
    ))
}

/// Run `gh auth token` and return stdout (trimmed). Returns an
/// `Err` when `gh` is missing or the command fails, which the
/// caller treats as "no token from gh".
fn gh_auth_token() -> Result<String, std::io::Error> {
    let output = Command::new("gh").args(["auth", "token"]).output()?;
    if !output.status.success() {
        return Err(std::io::Error::other("`gh auth token` exited non-zero"));
    }
    let token = String::from_utf8(output.stdout)
        .map_err(|e| std::io::Error::other(format!("`gh auth token` non-UTF-8 output: {e}")))?
        .trim()
        .to_string();
    Ok(token)
}

/// Run `git config --get remote.origin.url` in the current
/// directory and return stdout (trimmed). Returns `None` when git
/// isn't available, the working tree isn't a git repo, or the
/// remote isn't configured.
fn git_remote_origin_url() -> Option<String> {
    let output = Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!value.is_empty()).then_some(value)
}

/// Parse a git remote URL into `<owner>/<repo>`.
///
/// Handles both HTTPS (`https://github.com/owner/repo.git`) and
/// SSH (`git@github.com:owner/repo.git`) shapes; `.git` suffix and
/// trailing slashes are stripped. Returns `None` when the URL
/// doesn't decompose into at least two path segments.
fn parse_slug(url: &str) -> Option<String> {
    let url = url.trim();

    // SSH form: `git@host:owner/repo[.git]` — no scheme, the
    // delimiter between user@host and path is `:`. We detect this
    // by checking for `@…:` before the first `/`.
    let path = if let Some(scheme_end) = url.find("://") {
        let after_scheme = &url[scheme_end + 3..];
        after_scheme.split_once('/')?.1.to_string()
    } else if let Some(colon) = url.find(':') {
        url[colon + 1..].to_string()
    } else {
        return None;
    };

    let path = path.trim_end_matches('/').trim_start_matches('/');
    let (owner, rest) = path.split_once('/')?;
    let repo = rest
        .trim_end_matches('/')
        .strip_suffix(".git")
        .unwrap_or(rest);
    let repo = repo.trim_end_matches('/');
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_token_prefers_explicit_over_env() {
        temp_env::with_vars(
            [
                ("MERGIFY_TOKEN", Some("env-mergify")),
                ("GITHUB_TOKEN", Some("env-github")),
            ],
            || {
                assert_eq!(
                    resolve_token(Some("explicit-token")).unwrap(),
                    "explicit-token",
                );
            },
        );
    }

    #[test]
    fn resolve_token_falls_back_to_mergify_env() {
        temp_env::with_vars(
            [
                ("MERGIFY_TOKEN", Some("env-mergify")),
                ("GITHUB_TOKEN", Some("env-github")),
            ],
            || {
                assert_eq!(resolve_token(None).unwrap(), "env-mergify");
            },
        );
    }

    #[test]
    fn resolve_token_falls_back_to_github_env_when_mergify_unset() {
        temp_env::with_vars(
            [
                ("MERGIFY_TOKEN", None),
                ("GITHUB_TOKEN", Some("env-github")),
            ],
            || {
                assert_eq!(resolve_token(None).unwrap(), "env-github");
            },
        );
    }

    #[test]
    fn resolve_token_error_message_mentions_gh() {
        // When env vars are unset and `gh auth token` is unavailable
        // (or fails), the user-facing error must mention the gh
        // fallback so the user knows there's a third option.
        // Forcing PATH to a directory with no `gh` keeps the test
        // hermetic on machines that do have the GitHub CLI installed.
        temp_env::with_vars(
            [
                ("MERGIFY_TOKEN", None),
                ("GITHUB_TOKEN", None),
                ("PATH", Some("/nonexistent-directory-for-test")),
            ],
            || {
                let err = resolve_token(None).unwrap_err();
                let msg = err.to_string();
                assert!(msg.contains("MERGIFY_TOKEN"), "got {msg:?}");
                assert!(msg.contains("gh client"), "got {msg:?}");
            },
        );
    }

    #[test]
    fn resolve_api_url_default() {
        temp_env::with_var("MERGIFY_API_URL", None::<&str>, || {
            let url = resolve_api_url(None).unwrap();
            assert_eq!(url.as_str(), "https://api.mergify.com/");
        });
    }

    #[test]
    fn resolve_api_url_prefers_explicit() {
        temp_env::with_var("MERGIFY_API_URL", Some("https://from-env.example/"), || {
            let url = resolve_api_url(Some("https://explicit.example/")).unwrap();
            assert_eq!(url.as_str(), "https://explicit.example/");
        });
    }

    #[test]
    fn resolve_api_url_uses_env_var_when_explicit_empty() {
        temp_env::with_var("MERGIFY_API_URL", Some("https://from-env.example/"), || {
            let url = resolve_api_url(None).unwrap();
            assert_eq!(url.as_str(), "https://from-env.example/");
        });
    }

    #[test]
    fn resolve_api_url_rejects_garbage() {
        temp_env::with_var("MERGIFY_API_URL", None::<&str>, || {
            let err = resolve_api_url(Some("not a url")).unwrap_err();
            assert!(err.to_string().contains("invalid --api-url"));
        });
    }

    #[test]
    fn resolve_repository_prefers_explicit() {
        temp_env::with_var("GITHUB_REPOSITORY", Some("owner-from-env/repo"), || {
            assert_eq!(
                resolve_repository(Some("explicit/repo")).unwrap(),
                "explicit/repo",
            );
        });
    }

    #[test]
    fn resolve_repository_falls_back_to_env() {
        temp_env::with_var("GITHUB_REPOSITORY", Some("owner/repo"), || {
            assert_eq!(resolve_repository(None).unwrap(), "owner/repo");
        });
    }

    #[test]
    fn parse_slug_https_with_dot_git() {
        assert_eq!(
            parse_slug("https://github.com/owner/repo.git").as_deref(),
            Some("owner/repo"),
        );
    }

    #[test]
    fn parse_slug_https_without_dot_git() {
        assert_eq!(
            parse_slug("https://github.com/owner/repo").as_deref(),
            Some("owner/repo"),
        );
    }

    #[test]
    fn parse_slug_https_with_trailing_slash() {
        assert_eq!(
            parse_slug("https://github.com/owner/repo/").as_deref(),
            Some("owner/repo"),
        );
    }

    #[test]
    fn parse_slug_ssh_form() {
        assert_eq!(
            parse_slug("git@github.com:owner/repo.git").as_deref(),
            Some("owner/repo"),
        );
    }

    #[test]
    fn parse_slug_ssh_without_dot_git() {
        assert_eq!(
            parse_slug("git@github.com:owner/repo").as_deref(),
            Some("owner/repo"),
        );
    }

    #[test]
    fn parse_slug_rejects_empty_owner() {
        assert!(parse_slug("https://github.com//repo.git").is_none());
    }

    #[test]
    fn parse_slug_rejects_path_without_repo() {
        assert!(parse_slug("https://github.com/owner").is_none());
    }
}
