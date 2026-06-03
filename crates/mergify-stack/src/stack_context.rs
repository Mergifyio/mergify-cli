//! Stack-context resolution helpers.
//!
//! Shared by the GitHub-API-backed stack subcommands (`checkout`,
//! `sync`, `list`, `open`, `push`) — port of the per-command
//! preamble that Python's `mergify_cli/stack/cli.py` runs inside
//! the click group:
//!
//! - `github_server` from the `mergify-cli.github-server` git
//!   config key, falling back to `https://api.github.com/`.
//! - Repository `(owner, repo)` from either an explicit
//!   `--repository OWNER/REPO` argument or
//!   `git config remote.<remote>.url` parsed by [`parse_slug`].
//! - Default branch prefix from
//!   `mergify-cli.stack-branch-prefix`, falling back to
//!   `stack/<author>`.
//! - URL slug parser handling both HTTPS (`https://github.com/o/r`)
//!   and SSH (`git@github.com:o/r.git`) shapes.

use std::path::Path;
use std::process::Command;

use mergify_core::CliError;
use url::Url;

/// Owner + repository name pair, e.g. `("Mergifyio", "mergify-cli")`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSlug {
    pub owner: String,
    pub repo: String,
}

/// Parse a remote URL into `(owner, repo)`. Handles both HTTPS
/// (`https://github.com/owner/repo.git`) and SSH
/// (`git@github.com:owner/repo.git`) shapes. The trailing `.git`
/// suffix is stripped; a trailing `/` on the path is tolerated.
///
/// Mirrors `mergify_cli/utils.py::get_slug`.
pub fn parse_slug(url: &str) -> Result<RepoSlug, CliError> {
    // SSH shape: `user@host:owner/repo[.git]` — no `//` and the
    // path lives after the first `:`. Python's `urlparse` returns
    // empty netloc for this shape; we mirror by checking for `://`.
    let path = if url.contains("://") {
        // Validate via the `url` crate so we surface a clear error
        // on garbage input.
        let parsed = Url::parse(url).map_err(|e| {
            CliError::InvalidState(format!("could not parse remote URL '{url}': {e}"))
        })?;
        parsed
            .path()
            .trim_start_matches('/')
            .trim_end_matches('/')
            .to_string()
    } else {
        let (_user_host, path) = url.split_once(':').ok_or_else(|| {
            CliError::InvalidState(format!(
                "remote URL '{url}' is not parseable as HTTPS or SSH"
            ))
        })?;
        path.trim_end_matches('/').to_string()
    };
    let (owner, repo) = path.split_once('/').ok_or_else(|| {
        CliError::InvalidState(format!("remote URL '{url}' has no `<owner>/<repo>` path"))
    })?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    if owner.is_empty() || repo.is_empty() {
        return Err(CliError::InvalidState(format!(
            "remote URL '{url}' parses to an empty owner or repo"
        )));
    }
    Ok(RepoSlug {
        owner: owner.to_string(),
        repo: repo.to_string(),
    })
}

/// Resolve the repository from an explicit `--repository OWNER/REPO`
/// flag (preferred) or, when missing, by reading
/// `remote.<remote>.url` from git config and slug-parsing it.
pub fn resolve_repo(
    repo_dir: Option<&Path>,
    explicit_repository: Option<&str>,
    remote: &str,
) -> Result<RepoSlug, CliError> {
    if let Some(value) = explicit_repository {
        let (owner, repo) = value.split_once('/').ok_or_else(|| {
            CliError::InvalidState("--repository must be in the format 'owner/repo'".to_string())
        })?;
        if owner.is_empty() || repo.is_empty() {
            return Err(CliError::InvalidState(
                "--repository must be in the format 'owner/repo'".to_string(),
            ));
        }
        return Ok(RepoSlug {
            owner: owner.to_string(),
            repo: repo.to_string(),
        });
    }
    let key = format!("remote.{remote}.url");
    let url = run_git_capture(repo_dir, &["config", "--get", &key])?;
    parse_slug(&url)
}

/// Resolve `mergify-cli.github-server` from git config, falling
/// back to `https://api.github.com/`. The Python implementation
/// rewrites the scheme to `https` and tacks on `/api/v3` when the
/// host is not `api.github.com` (so GitHub Enterprise users can
/// configure just the host).
pub fn resolve_github_server(repo_dir: Option<&Path>) -> Result<Url, CliError> {
    let configured = run_git_capture(repo_dir, &["config", "--get", "mergify-cli.github-server"])
        .unwrap_or_default();
    let raw = if configured.is_empty() {
        "https://api.github.com/".to_string()
    } else {
        configured
    };
    let mut url = Url::parse(&raw).map_err(|e| {
        CliError::InvalidState(format!("invalid mergify-cli.github-server '{raw}': {e}"))
    })?;
    // Force scheme to https — Python's `_replace(scheme="https")`.
    if url.scheme() != "https" {
        url.set_scheme("https")
            .map_err(|()| CliError::InvalidState(format!("could not coerce '{raw}' to https")))?;
    }
    let host = url.host_str().unwrap_or("").to_string();
    if host == "api.github.com" {
        url.set_path("");
    } else {
        url.set_path("/api/v3");
    }
    Ok(url)
}

/// Resolve the default branch prefix from
/// `mergify-cli.stack-branch-prefix`, falling back to
/// `stack/<author>`. Mirrors `utils.get_default_branch_prefix`.
#[must_use]
pub fn resolve_default_branch_prefix(repo_dir: Option<&Path>, author: &str) -> String {
    let configured = run_git_capture(
        repo_dir,
        &["config", "--get", "mergify-cli.stack-branch-prefix"],
    )
    .unwrap_or_default();
    if configured.is_empty() {
        format!("stack/{author}")
    } else {
        configured
    }
}

fn run_git_capture(repo_dir: Option<&Path>, args: &[&str]) -> Result<String, CliError> {
    let mut cmd = Command::new("git");
    if let Some(dir) = repo_dir {
        cmd.arg("-C").arg(dir);
    }
    cmd.args(args);
    let output = cmd
        .output()
        .map_err(|e| CliError::Generic(format!("failed to spawn `git {}`: {e}", args.join(" "))))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(CliError::Generic(if stderr.is_empty() {
            format!("`git {}` failed", args.join(" "))
        } else {
            stderr
        }));
    }
    let stdout = String::from_utf8(output.stdout).map_err(|e| {
        CliError::Generic(format!("`git {}` output is not UTF-8: {e}", args.join(" ")))
    })?;
    Ok(stdout.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_https_url() {
        let slug = parse_slug("https://github.com/owner/repo.git").unwrap();
        assert_eq!(slug.owner, "owner");
        assert_eq!(slug.repo, "repo");
    }

    #[test]
    fn parses_https_url_without_git_suffix() {
        let slug = parse_slug("https://github.com/owner/repo").unwrap();
        assert_eq!(slug.repo, "repo");
    }

    #[test]
    fn parses_https_url_with_trailing_slash() {
        let slug = parse_slug("https://github.com/owner/repo/").unwrap();
        assert_eq!(slug.repo, "repo");
    }

    #[test]
    fn parses_ssh_url() {
        let slug = parse_slug("git@github.com:owner/repo.git").unwrap();
        assert_eq!(slug.owner, "owner");
        assert_eq!(slug.repo, "repo");
    }

    #[test]
    fn parses_ssh_url_without_git_suffix() {
        let slug = parse_slug("git@github.com:owner/repo").unwrap();
        assert_eq!(slug.repo, "repo");
    }

    #[test]
    fn invalid_url_errors() {
        let err = parse_slug("not-a-url").unwrap_err();
        match err {
            CliError::InvalidState(msg) => {
                assert!(
                    msg.contains("not parseable") || msg.contains("invalid"),
                    "got: {msg}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn empty_components_error() {
        let err = parse_slug("https://github.com//repo").unwrap_err();
        match err {
            CliError::InvalidState(_) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
}
