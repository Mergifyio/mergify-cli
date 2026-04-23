//! Resolve `--token`, `--api-url`, `--repository` with the same
//! env-variable fallbacks as Python.
//!
//! Duplicates the same helpers that live in `mergify-config::simulate`
//! and `mergify-ci::scopes_send` today. Once a fourth command
//! needs them, they factor into `mergify-core::auth`.

use std::env;

use mergify_core::CliError;
use url::Url;

const DEFAULT_API_URL: &str = "https://api.mergify.com";

/// Resolve the Mergify API bearer token.
///
/// Precedence: explicit `--token`, then `MERGIFY_TOKEN`, then
/// `GITHUB_TOKEN`. Errors out when none of those are set.
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
    Err(CliError::Configuration(
        "please set the 'MERGIFY_TOKEN' or 'GITHUB_TOKEN' environment variable, \
         or pass --token explicitly"
            .to_string(),
    ))
}

/// Resolve the Mergify API base URL. Falls back to `MERGIFY_API_URL`
/// env var, then to the default `https://api.mergify.com`.
pub fn resolve_api_url(explicit: Option<&str>) -> Result<Url, CliError> {
    let raw = explicit
        .map(str::to_string)
        .or_else(|| env::var("MERGIFY_API_URL").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_API_URL.to_string());
    Url::parse(&raw).map_err(|e| CliError::Configuration(format!("invalid --api-url {raw:?}: {e}")))
}

/// Resolve the repository (owner/repo) identifier. Falls back to
/// the `GITHUB_REPOSITORY` env var.
pub fn resolve_repository(explicit: Option<&str>) -> Result<String, CliError> {
    if let Some(value) = explicit.filter(|s| !s.is_empty()) {
        return Ok(value.to_string());
    }
    env::var("GITHUB_REPOSITORY")
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            CliError::Configuration(
                "--repository not provided and GITHUB_REPOSITORY env var is unset".to_string(),
            )
        })
}
