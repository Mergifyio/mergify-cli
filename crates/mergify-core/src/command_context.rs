//! Per-command "resolved context" + Mergify HTTP client builder.
//!
//! Every Mergify-API command starts the same way: resolve the
//! repository slug, the bearer token, and the API URL via the
//! standard fallback chain (flag → env → `gh auth token` / git
//! remote / default), then build a typed [`HttpClient`] from them.
//! [`CommandContext`] bundles those three pieces with a
//! `mergify_client()` builder so the prelude shrinks from a four-line
//! ritual to two:
//!
//! ```ignore
//! let ctx = CommandContext::resolve(opts.repository, opts.token, opts.api_url)?;
//! let client = ctx.mergify_client()?;
//! ```
//!
//! Specialized commands that don't fit the shape (`config validate`
//! needs no repository; `ci scopes-send` resolves the repo from CI
//! env; `config simulate` derives it from a PR URL) keep wiring up
//! the lower-level [`auth::resolve_*`] / [`HttpClient::new`] calls
//! by hand.
//!
//! [`auth::resolve_*`]: crate::auth

use url::Url;

use crate::auth;
use crate::error::CliError;
use crate::http::ApiFlavor;
use crate::http::Client as HttpClient;

/// Resolved repository / token / API URL for a Mergify-API command.
pub struct CommandContext {
    pub repository: String,
    pub token: String,
    pub api_url: Url,
}

impl CommandContext {
    /// Resolve all three pieces of context using the standard
    /// fallback chain. Used by the `queue` and `freeze` command
    /// families — every member of those groups needs the same
    /// shape.
    ///
    /// # Errors
    ///
    /// Surfaces the first resolution failure as
    /// [`CliError::Configuration`].
    pub fn resolve(
        repository: Option<&str>,
        token: Option<&str>,
        api_url: Option<&str>,
    ) -> Result<Self, CliError> {
        Ok(Self {
            repository: auth::resolve_repository(repository)?,
            token: auth::resolve_token(token)?,
            api_url: auth::resolve_api_url(api_url)?,
        })
    }

    /// Build a Mergify-flavored [`HttpClient`] from this context.
    /// Clones the API URL so the [`CommandContext`] stays usable
    /// afterwards — callers typically still need `self.repository`
    /// to format URL paths.
    pub fn mergify_client(&self) -> Result<HttpClient, CliError> {
        HttpClient::new(self.api_url.clone(), &self.token, ApiFlavor::Mergify)
    }
}
