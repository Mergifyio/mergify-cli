//! Resolve `--token`, `--api-url`, and `--repository`.
//!
//! The Python CLI resolved one token — `--token` → `MERGIFY_TOKEN`
//! → `GITHUB_TOKEN` → `gh auth token` — and sent it to two
//! unrelated services. It no longer does.
//!
//! [`resolve_mergify_token`] answers for the Mergify API:
//!
//! 1. `--token`
//! 2. `MERGIFY_TOKEN`
//! 3. **the credential `mergify auth login` stored for this API URL**
//! 4. `GITHUB_TOKEN` — deprecated, warns once
//! 5. `gh auth token` — deprecated, warns once
//!
//! The stored credential sits *above* the two GitHub fallbacks on
//! purpose: a `GITHUB_TOKEN` left in a shell must not silently
//! override a login the user performed. It sits *below* `--token`
//! and `MERGIFY_TOKEN` so a CI job keeps working unchanged, and
//! neither of those warns.
//!
//! [`resolve_github_token`] answers for `api.github.com` — the
//! `stack` command group — and is exactly the chain above this
//! change: no stored credential, no warning, nothing new sent to
//! GitHub.
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

use std::process::Command;
use std::sync::Once;

use url::Url;

use crate::CliError;
use crate::credentials::CredentialStore;
use crate::env::var_non_empty;

const DEFAULT_API_URL: &str = "https://api.mergify.com";

/// Prefix of a Mergify-issued user token — the credential
/// `mergify auth login` mints. Registered with GitHub's secret
/// scanning as ours, so nothing shaped like this is a GitHub
/// credential.
const MERGIFY_USER_TOKEN_PREFIX: &str = "mut_";

/// Which Mergify credential a command's routes actually accept.
///
/// Not decoration: the `ci` routes are declared
/// `enable_auth_methods("ci_application_key")` server-side and
/// refuse a user credential by design, because a CI runner holds an
/// organization key rather than somebody's personal login. So the
/// stored credential is not offered to them, and the deprecation
/// warning they print names the remedy that would actually work.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Audience {
    /// Routes that take the per-user credential `mergify auth login`
    /// mints: `queue`, `freeze`, `events`, `tests`, `config
    /// simulate`.
    User,
    /// The `ci` routes, which require an organization application
    /// key.
    ApplicationKey,
}

/// Where a resolved Mergify token came from.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TokenSource {
    Explicit,
    MergifyTokenEnv,
    /// The credential `mergify auth login` stored for this API URL.
    Stored,
    GitHubTokenEnv,
    GhCli,
}

/// The environment variable a Mergify command would use in
/// preference to the stored credential, if one is set.
///
/// `auth status` reports it, so "logged in" never silently means
/// "logged in, and overridden by something in your shell". Lives
/// here rather than in the `auth` crate because the precedence it
/// describes is this module's, and two copies of it would drift.
#[must_use]
pub fn overriding_env_var() -> Option<&'static str> {
    var_non_empty("MERGIFY_TOKEN").map(|_| "MERGIFY_TOKEN")
}

/// Resolve the bearer token for **Mergify API** calls.
///
/// Precedence: `--token`, `MERGIFY_TOKEN`, the stored credential for
/// `api_url`, `GITHUB_TOKEN`, `gh auth token`. The last two are
/// deprecated and warn once per process on stderr. Errors when none
/// of them produce a non-empty value.
pub fn resolve_mergify_token(
    explicit: Option<&str>,
    api_url: &Url,
    audience: Audience,
) -> Result<String, CliError> {
    let store = CredentialStore::discover();
    let resolved = resolve_mergify_token_with(explicit, api_url, audience, &store)?;
    if let Some(notice) = deprecation_notice(resolved.source, audience) {
        warn_once(&notice);
    }
    Ok(resolved.token)
}

/// A resolved Mergify token and the step of the chain it came from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedToken {
    pub token: String,
    pub source: TokenSource,
}

/// The chain itself, against an explicit store. Private because a
/// caller has no business choosing a credential store; the tests
/// use it so they never read — or write — the developer's keychain.
fn resolve_mergify_token_with(
    explicit: Option<&str>,
    api_url: &Url,
    audience: Audience,
    store: &CredentialStore,
) -> Result<ResolvedToken, CliError> {
    if let Some(value) = explicit.filter(|s| !s.is_empty()) {
        return Ok(ResolvedToken {
            token: value.to_string(),
            source: TokenSource::Explicit,
        });
    }
    if let Some(value) = var_non_empty("MERGIFY_TOKEN") {
        return Ok(ResolvedToken {
            token: value,
            source: TokenSource::MergifyTokenEnv,
        });
    }
    // Skipped for the `ci` routes: they refuse a user credential, so
    // offering one there would replace a legible "set MERGIFY_TOKEN"
    // failure with an authentication error.
    if audience == Audience::User {
        match store.get(api_url) {
            Ok(Some(stored)) => {
                return Ok(ResolvedToken {
                    token: stored.credential.token,
                    source: TokenSource::Stored,
                });
            }
            Ok(None) => {}
            // A store that cannot be read is worth a line and not
            // worth failing on: the chain has two more steps.
            Err(e) => tracing::debug!(error = %e, "could not read the stored credential"),
        }
    }
    if let Some(value) = var_non_empty("GITHUB_TOKEN") {
        return Ok(ResolvedToken {
            token: value,
            source: TokenSource::GitHubTokenEnv,
        });
    }
    if let Ok(token) = gh_auth_token()
        && !token.is_empty()
    {
        return Ok(ResolvedToken {
            token,
            source: TokenSource::GhCli,
        });
    }
    Err(CliError::Configuration(no_credential_message(audience)))
}

/// What to tell a user who has no credential at all. The remedy
/// differs by audience for the same reason the deprecation warning
/// does: `mergify auth login` mints a credential the `ci` routes
/// refuse.
fn no_credential_message(audience: Audience) -> String {
    match audience {
        Audience::User => "no Mergify credential found. Run `mergify auth login`, or set the \
             'MERGIFY_TOKEN' environment variable."
            .to_string(),
        Audience::ApplicationKey => "no Mergify credential found. Set the 'MERGIFY_TOKEN' \
             environment variable to a Mergify application key."
            .to_string(),
    }
}

/// The deprecation notice for `source`, or `None` when the
/// credential is not deprecated.
fn deprecation_notice(source: TokenSource, audience: Audience) -> Option<String> {
    let what = match source {
        TokenSource::GitHubTokenEnv => "the GITHUB_TOKEN environment variable",
        TokenSource::GhCli => "the token from `gh auth token`",
        TokenSource::Explicit | TokenSource::MergifyTokenEnv | TokenSource::Stored => return None,
    };
    // The `ci` routes refuse the credential `auth login` mints, so
    // sending a CI runner to that command would be sending it to a
    // dead end.
    let remedy = match audience {
        Audience::User => "Run `mergify auth login` instead.",
        Audience::ApplicationKey => "Set MERGIFY_TOKEN to a Mergify application key instead.",
    };
    Some(format!(
        "mergify: warning: {what} is deprecated as a Mergify API credential and will stop \
         working in a future release.\n  {remedy}",
    ))
}

/// Print `message` to stderr the first time only.
///
/// Once per process, not once per call: `ci junit-process` uploads a
/// batch per file, and a warning repeated per API call would bury
/// the output it is warning about.
fn warn_once(message: &str) {
    static WARNED: Once = Once::new();
    WARNED.call_once(|| eprintln!("{message}"));
}

/// Resolve the bearer token for **GitHub REST** calls — the
/// `ApiFlavor::GitHub` client the `stack` command group talks to
/// `api.github.com` with.
///
/// Precedence: explicit `--token`, then `MERGIFY_TOKEN`, then
/// `GITHUB_TOKEN`, then the output of `gh auth token`. Deliberately
/// untouched by the Mergify side's deprecation: `stack` legitimately
/// needs a GitHub token, and warning it off the only credentials
/// GitHub accepts would be nonsense.
pub fn resolve_github_token(explicit: Option<&str>) -> Result<String, CliError> {
    if let Some(value) = explicit.filter(|s| !s.is_empty()) {
        return Ok(value.to_string());
    }
    let mut skipped_mergify_token = false;
    for env_name in ["MERGIFY_TOKEN", "GITHUB_TOKEN"] {
        let Some(value) = var_non_empty(env_name) else {
            continue;
        };
        // `MERGIFY_TOKEN` is the natural place to put the token
        // `mergify auth login` mints, and `stack` reads the same
        // variable for its GitHub calls. GitHub would answer `401 Bad
        // credentials`, which says nothing about why. Skipping it
        // costs nothing: this prefix is Mergify's, registered with
        // GitHub's own secret scanning, so a value carrying it was
        // never going to authenticate there.
        if value.starts_with(MERGIFY_USER_TOKEN_PREFIX) {
            tracing::debug!(
                env_name,
                "holds a Mergify user token, which GitHub cannot accept; trying the next \
                 credential"
            );
            skipped_mergify_token = true;
            continue;
        }
        return Ok(value);
    }
    if let Ok(token) = gh_auth_token()
        && !token.is_empty()
    {
        return Ok(token);
    }
    // Telling someone to set `MERGIFY_TOKEN` when they have set it,
    // and it was skipped two lines above, is the worst version of
    // this message: the reason is real and only visible at `-vv`.
    if skipped_mergify_token {
        return Err(CliError::Configuration(
            "MERGIFY_TOKEN holds a Mergify-issued token, which GitHub does not accept, and \
             `mergify stack` talks to GitHub. Set 'GITHUB_TOKEN', or make sure that the gh \
             client is installed and you are authenticated."
                .to_string(),
        ));
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
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| var_non_empty("MERGIFY_API_URL"))
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
    if let Some(value) = var_non_empty("GITHUB_REPOSITORY") {
        return Ok(value);
    }
    if let Some(slug) = repository_from_git_remote() {
        return Ok(slug);
    }
    Err(CliError::Configuration(
        "--repository not provided, GITHUB_REPOSITORY env var is unset, and \
         the local git config has no usable `remote.origin.url`"
            .to_string(),
    ))
}

/// Repository slug (`<owner>/<repo>`) parsed from the local
/// `git config --get remote.origin.url`, or `None` when the working
/// tree isn't a git repo or has no usable `remote.origin.url`.
///
/// Exposed so `mergify-ci`'s CI-aware resolver can share the same
/// git-remote fallback instead of reimplementing it.
#[must_use]
pub fn repository_from_git_remote() -> Option<String> {
    parse_slug(&git_remote_origin_url()?)
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

    // A scheme (`://`) means an HTTPS-style URL: the slug is the
    // path after the host. Anything without a scheme is treated
    // as the SSH form `git@host:owner/repo[.git]`: the slug is
    // whatever follows the first `:`.
    let path = if let Some(scheme_end) = url.find("://") {
        let after_scheme = &url[scheme_end + 3..];
        after_scheme.split_once('/')?.1.to_string()
    } else {
        let colon = url.find(':')?;
        url[colon + 1..].to_string()
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

    /// A store that cannot reach the developer's real keychain,
    /// optionally holding a credential for [`api_url`].
    fn store_with(token: Option<&str>) -> (tempfile::TempDir, CredentialStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::file_at(dir.path().join("credentials.json"));
        if let Some(token) = token {
            store
                .set(
                    &api_url(),
                    &crate::credentials::Credential {
                        token: token.to_string(),
                        expires_at: None,
                    },
                )
                .unwrap();
        }
        (dir, store)
    }

    fn api_url() -> Url {
        Url::parse("https://api.mergify.com").unwrap()
    }

    /// The chain, with no keychain anywhere near it. `None` for the
    /// store means "this machine has no credential store at all".
    fn resolve(
        explicit: Option<&str>,
        audience: Audience,
        store: &CredentialStore,
    ) -> Result<ResolvedToken, CliError> {
        resolve_mergify_token_with(explicit, &api_url(), audience, store)
    }

    #[test]
    fn resolve_mergify_token_prefers_explicit_over_everything() {
        let (_dir, store) = store_with(Some("stored"));
        temp_env::with_vars(
            [
                ("MERGIFY_TOKEN", Some("env-mergify")),
                ("GITHUB_TOKEN", Some("env-github")),
            ],
            || {
                let resolved = resolve(Some("explicit-token"), Audience::User, &store).unwrap();
                assert_eq!(resolved.token, "explicit-token");
                assert_eq!(resolved.source, TokenSource::Explicit);
            },
        );
    }

    // `MERGIFY_TOKEN` stays above the stored credential so a CI job
    // that sets it keeps working exactly as it did.
    #[test]
    fn resolve_mergify_token_prefers_the_mergify_env_var_over_the_store() {
        let (_dir, store) = store_with(Some("stored"));
        temp_env::with_vars(
            [
                ("MERGIFY_TOKEN", Some("env-mergify")),
                ("GITHUB_TOKEN", Some("env-github")),
            ],
            || {
                let resolved = resolve(None, Audience::User, &store).unwrap();
                assert_eq!(resolved.token, "env-mergify");
                assert_eq!(resolved.source, TokenSource::MergifyTokenEnv);
            },
        );
    }

    // The whole point of the new step: a `GITHUB_TOKEN` lying around
    // in a shell must not silently override a login the user
    // performed.
    #[test]
    fn the_stored_credential_beats_github_token() {
        let (_dir, store) = store_with(Some("mut_stored"));
        temp_env::with_vars(
            [
                ("MERGIFY_TOKEN", None),
                ("GITHUB_TOKEN", Some("env-github")),
            ],
            || {
                let resolved = resolve(None, Audience::User, &store).unwrap();
                assert_eq!(resolved.token, "mut_stored");
                assert_eq!(resolved.source, TokenSource::Stored);
            },
        );
    }

    // The `ci` routes refuse a user credential server-side, so
    // handing them one would turn "set MERGIFY_TOKEN" into an
    // authentication error.
    #[test]
    fn the_ci_audience_never_uses_the_stored_credential() {
        let (_dir, store) = store_with(Some("mut_stored"));
        temp_env::with_vars(
            [
                ("MERGIFY_TOKEN", None),
                ("GITHUB_TOKEN", Some("env-github")),
            ],
            || {
                let resolved = resolve(None, Audience::ApplicationKey, &store).unwrap();
                assert_eq!(resolved.token, "env-github");
                assert_eq!(resolved.source, TokenSource::GitHubTokenEnv);
            },
        );
    }

    #[test]
    fn resolve_mergify_token_falls_back_to_github_env_when_nothing_is_stored() {
        let (_dir, store) = store_with(None);
        temp_env::with_vars(
            [
                ("MERGIFY_TOKEN", None),
                ("GITHUB_TOKEN", Some("env-github")),
            ],
            || {
                let resolved = resolve(None, Audience::User, &store).unwrap();
                assert_eq!(resolved.token, "env-github");
                assert_eq!(resolved.source, TokenSource::GitHubTokenEnv);
            },
        );
    }

    // A credential stored for one deployment is not a credential for
    // another.
    #[test]
    fn a_credential_stored_for_another_deployment_is_not_used() {
        let (_dir, store) = store_with(Some("mut_stored"));
        // `PATH` too: without it a developer machine with an
        // authenticated `gh` answers the last step of the chain and
        // the test asserts on the wrong thing.
        temp_env::with_vars(
            [
                ("MERGIFY_TOKEN", None),
                ("GITHUB_TOKEN", None),
                ("PATH", Some("/nonexistent-directory-for-test")),
            ],
            || {
                let other = Url::parse("https://mergify.internal.example/api").unwrap();
                let err =
                    resolve_mergify_token_with(None, &other, Audience::User, &store).unwrap_err();
                assert!(
                    err.to_string().contains("no Mergify credential"),
                    "got {err}"
                );
            },
        );
    }

    #[test]
    fn resolve_mergify_token_error_names_the_command_that_fixes_it() {
        // Forcing PATH to a directory with no `gh` keeps the test
        // hermetic on machines that do have the GitHub CLI installed.
        let (_dir, store) = store_with(None);
        temp_env::with_vars(
            [
                ("MERGIFY_TOKEN", None),
                ("GITHUB_TOKEN", None),
                ("PATH", Some("/nonexistent-directory-for-test")),
            ],
            || {
                let err = resolve(None, Audience::User, &store).unwrap_err();
                let msg = err.to_string();
                assert!(msg.contains("mergify auth login"), "got {msg:?}");
                assert!(msg.contains("MERGIFY_TOKEN"), "got {msg:?}");
            },
        );
    }

    // A `ci` command cannot be fixed by `auth login`; the message it
    // gets must not send it there.
    #[test]
    fn the_ci_audience_is_not_told_to_run_auth_login() {
        let (_dir, store) = store_with(None);
        temp_env::with_vars(
            [
                ("MERGIFY_TOKEN", None),
                ("GITHUB_TOKEN", None),
                ("PATH", Some("/nonexistent-directory-for-test")),
            ],
            || {
                let err = resolve(None, Audience::ApplicationKey, &store).unwrap_err();
                let msg = err.to_string();
                assert!(!msg.contains("auth login"), "got {msg:?}");
                assert!(msg.contains("application key"), "got {msg:?}");
            },
        );
    }

    #[test]
    fn only_the_two_github_credentials_are_deprecated() {
        for source in [
            TokenSource::Explicit,
            TokenSource::MergifyTokenEnv,
            TokenSource::Stored,
        ] {
            assert_eq!(
                deprecation_notice(source, Audience::User),
                None,
                "{source:?} must not warn",
            );
        }
    }

    #[test]
    fn the_deprecation_notice_names_the_credential_and_the_remedy() {
        let notice = deprecation_notice(TokenSource::GitHubTokenEnv, Audience::User).unwrap();
        assert!(notice.contains("GITHUB_TOKEN"), "got {notice:?}");
        assert!(notice.contains("deprecated"), "got {notice:?}");
        assert!(notice.contains("mergify auth login"), "got {notice:?}");

        let notice = deprecation_notice(TokenSource::GhCli, Audience::User).unwrap();
        assert!(notice.contains("gh auth token"), "got {notice:?}");
    }

    // Same deprecation, different remedy: `auth login` mints a
    // credential the `ci` routes refuse.
    #[test]
    fn the_ci_deprecation_notice_points_at_an_application_key() {
        let notice =
            deprecation_notice(TokenSource::GitHubTokenEnv, Audience::ApplicationKey).unwrap();
        assert!(notice.contains("GITHUB_TOKEN"), "got {notice:?}");
        assert!(!notice.contains("auth login"), "got {notice:?}");
        assert!(notice.contains("application key"), "got {notice:?}");
    }

    // `resolve_mergify_token` is the function every command calls;
    // the tests above all exercise the private chain behind it.
    // Nothing here can capture the deprecation warning it prints —
    // that goes to the process's stderr through a `Once` — but the
    // wrapper itself must at least be wired to the chain.
    #[test]
    fn the_public_resolver_answers_the_chain() {
        temp_env::with_vars(
            [
                ("MERGIFY_TOKEN", Some("env-mergify")),
                ("GITHUB_TOKEN", None),
            ],
            || {
                assert_eq!(
                    resolve_mergify_token(None, &api_url(), Audience::User).unwrap(),
                    "env-mergify",
                );
            },
        );
    }

    #[test]
    fn overriding_env_var_reports_what_outranks_the_stored_credential() {
        temp_env::with_var("MERGIFY_TOKEN", Some("env-mergify"), || {
            assert_eq!(overriding_env_var(), Some("MERGIFY_TOKEN"));
        });
        temp_env::with_var("MERGIFY_TOKEN", None::<&str>, || {
            assert_eq!(overriding_env_var(), None);
        });
    }

    // `MERGIFY_TOKEN` is the natural place to put a credential from
    // `mergify auth login`, and `stack` reads the same variable.
    // GitHub would answer `401 Bad credentials`, which says nothing
    // about why.
    #[test]
    fn resolve_github_token_skips_a_mergify_user_token() {
        temp_env::with_vars(
            [
                ("MERGIFY_TOKEN", Some("mut_from_auth_login")),
                ("GITHUB_TOKEN", Some("env-github")),
            ],
            || {
                assert_eq!(resolve_github_token(None).unwrap(), "env-github");
            },
        );
    }

    // Being skipped has to reach the failure, not only `-vv`:
    // otherwise `stack push` tells a user to set the variable they
    // set, for a reason they cannot see.
    #[test]
    fn resolve_github_token_says_why_it_skipped_the_mergify_token() {
        temp_env::with_vars(
            [
                ("MERGIFY_TOKEN", Some("mut_from_auth_login")),
                ("GITHUB_TOKEN", None),
                ("PATH", Some("/nonexistent-directory-for-test")),
            ],
            || {
                let err = resolve_github_token(None).unwrap_err();
                let message = err.to_string();
                assert!(
                    message.contains("GitHub does not accept"),
                    "got {message:?}",
                );
                assert!(message.contains("GITHUB_TOKEN"), "got {message:?}");
            },
        );
    }

    // An explicit `--token` is not second-guessed: the user aimed it
    // at this command, and silently using something else would be
    // the more surprising failure.
    #[test]
    fn resolve_github_token_still_honours_an_explicit_mergify_token() {
        temp_env::with_var("GITHUB_TOKEN", Some("env-github"), || {
            assert_eq!(
                resolve_github_token(Some("mut_explicit")).unwrap(),
                "mut_explicit",
            );
        });
    }

    // The GitHub resolver is what `stack` sends to `api.github.com`.
    // Pinning its whole chain here — rather than asserting it equals
    // whatever the Mergify one returns — is the point: the two
    // diverge on the Mergify side, and this test has to keep failing
    // if that divergence ever reaches the GitHub side.
    #[test]
    fn resolve_github_token_prefers_explicit_over_env() {
        temp_env::with_vars(
            [
                ("MERGIFY_TOKEN", Some("env-mergify")),
                ("GITHUB_TOKEN", Some("env-github")),
            ],
            || {
                assert_eq!(
                    resolve_github_token(Some("explicit-token")).unwrap(),
                    "explicit-token",
                );
            },
        );
    }

    #[test]
    fn resolve_github_token_falls_back_to_mergify_env() {
        temp_env::with_vars(
            [
                ("MERGIFY_TOKEN", Some("env-mergify")),
                ("GITHUB_TOKEN", Some("env-github")),
            ],
            || {
                assert_eq!(resolve_github_token(None).unwrap(), "env-mergify");
            },
        );
    }

    #[test]
    fn resolve_github_token_falls_back_to_github_env_when_mergify_unset() {
        temp_env::with_vars(
            [
                ("MERGIFY_TOKEN", None),
                ("GITHUB_TOKEN", Some("env-github")),
            ],
            || {
                assert_eq!(resolve_github_token(None).unwrap(), "env-github");
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
