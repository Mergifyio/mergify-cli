//! Parse a GitHub-style pull-request URL into its parts.
//!
//! Shared by every command that takes a PR URL on the command line
//! (`config simulate`, `ci queue-info`). Lives in `mergify-core`
//! next to `auth`/`http` rather than in any one command crate so
//! both call sites use a single parser instead of copies.

/// The `(host, owner/repo, number)` triple parsed from a pull-request
/// URL.
///
/// `host` is kept so callers that talk to the GitHub API (rather than
/// the Mergify API) can derive the right API base — `github.com` vs a
/// GitHub Enterprise Server host. Callers that don't need it (e.g.
/// `config simulate`, which always hits the Mergify API) simply
/// ignore the field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PullRequestRef {
    pub host: String,
    pub repository: String,
    pub pull_number: u64,
}

/// Clap value-parser for a positional PR URL argument.
///
/// Returning `Err(String)` makes clap exit with status 2 (argument
/// validation error) rather than our CLI's `ConfigurationError` —
/// matching the Python CLI's behavior where `_parse_pr_url` raises
/// `click.BadParameter` (also exit 2).
///
/// # Errors
///
/// Returns a human-readable message when `url` is not a valid
/// GitHub-style pull request URL.
pub fn parse_pr_url(url: &str) -> Result<PullRequestRef, String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| format!("Invalid pull request URL: {url}"))?;
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() != 5 || parts[3] != "pull" {
        return Err(format!("Invalid pull request URL: {url}"));
    }
    let [host, owner, repo, _pull, number] = [parts[0], parts[1], parts[2], parts[3], parts[4]];
    if host.is_empty() || owner.is_empty() || repo.is_empty() {
        return Err(format!("Invalid pull request URL: {url}"));
    }
    // A real GitHub PR URL has a bare host with no userinfo. Reject
    // `user@host` shapes: a host segment is later used as the GitHub
    // API authority (see `mergify-ci`'s `github_api_base`), and
    // `https://github.com@evil.com/...` parses to host `evil.com` with
    // `github.com` as decoy userinfo — which would send the bearer
    // token to the attacker host.
    if host.contains('@') {
        return Err(format!("Invalid pull request URL: {url}"));
    }
    let pull_number: u64 = number
        .parse()
        .map_err(|_| format!("Invalid pull request URL: {url}"))?;
    Ok(PullRequestRef {
        host: host.to_string(),
        repository: format!("{owner}/{repo}"),
        pull_number,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pr_url_accepts_canonical_github_url() {
        let got = parse_pr_url("https://github.com/owner/repo/pull/42").unwrap();
        assert_eq!(got.host, "github.com");
        assert_eq!(got.repository, "owner/repo");
        assert_eq!(got.pull_number, 42);
    }

    #[test]
    fn parse_pr_url_keeps_ghes_host() {
        let got = parse_pr_url("https://ghe.example.com/owner/repo/pull/7").unwrap();
        assert_eq!(got.host, "ghe.example.com");
        assert_eq!(got.repository, "owner/repo");
        assert_eq!(got.pull_number, 7);
    }

    #[test]
    fn parse_pr_url_rejects_non_pull_path() {
        assert!(parse_pr_url("https://github.com/owner/repo/issues/42").is_err());
    }

    #[test]
    fn parse_pr_url_rejects_trailing_segments() {
        assert!(parse_pr_url("https://github.com/owner/repo/pull/42/files").is_err());
    }

    #[test]
    fn parse_pr_url_rejects_non_numeric_pull_number() {
        assert!(parse_pr_url("https://github.com/owner/repo/pull/abc").is_err());
    }

    #[test]
    fn parse_pr_url_rejects_missing_scheme() {
        assert!(parse_pr_url("github.com/owner/repo/pull/42").is_err());
    }

    #[test]
    fn parse_pr_url_rejects_empty_owner() {
        assert!(parse_pr_url("https://github.com//repo/pull/42").is_err());
    }

    #[test]
    fn parse_pr_url_rejects_userinfo_host() {
        // `github.com@evil.com` parses (via Url) to host `evil.com`
        // with `github.com` as userinfo — rejecting `@` keeps a
        // crafted link from redirecting the GitHub API base (and the
        // bearer token) to an attacker host.
        assert!(parse_pr_url("https://github.com@evil.com/owner/repo/pull/1").is_err());
    }
}
