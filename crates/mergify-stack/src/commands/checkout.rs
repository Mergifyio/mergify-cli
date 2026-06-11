//! `mergify stack checkout <NAME>` — fetch a stack of pull
//! requests from GitHub and create a local branch tracking the
//! leaf head.
//!
//! Port of `mergify_cli/stack/checkout.py::stack_checkout`. The
//! flow:
//!
//! 1. Resolve author (falls back to `GET /user`).
//! 2. Normalise the user-supplied `NAME` — strip any trailing
//!    `/Ixxxx…` Change-Id suffix and any leading
//!    `<branch_prefix>/` so users can paste a branch ref verbatim.
//! 3. Search GitHub for the stack's PRs (via
//!    [`crate::remote_changes::get_remote_changes`]).
//! 4. Link open PRs into a single chain via their `head.ref` →
//!    `base.ref` pointers, find the root (the PR whose `base.ref`
//!    is *outside* the stack — i.e. doesn't start with the stack
//!    branch prefix), walk up to the leaf.
//! 5. Print the chain. When not `--dry-run`, `git fetch` the leaf
//!    head, `git checkout -b <local>` on it, set upstream
//!    tracking to the root's base.

use std::path::Path;

use crate::git::run_git_silent as run_git;

use mergify_core::CliError;
use mergify_core::HttpClient;
use serde_json::Value;

use crate::change_id;
use crate::remote_changes::{self, RemoteChange};

/// Pull-request summary surfaced to the caller for rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullSummary {
    pub number: u64,
    pub title: String,
    pub html_url: String,
    pub base_ref: String,
    pub head_ref: String,
}

#[derive(Debug, Clone)]
pub enum Outcome {
    /// Stack discovered. `chain` is base→leaf order. `created`
    /// indicates whether a local branch was actually checked out
    /// (false for `--dry-run`).
    CheckedOut {
        chain: Vec<PullSummary>,
        created: bool,
        local_branch: String,
        upstream: String,
    },
    NoStackedPrs,
}

pub struct Options<'a> {
    pub repo_dir: Option<&'a Path>,
    pub client: &'a HttpClient,
    pub user: &'a str,
    pub repo: &'a str,
    pub author: &'a str,
    /// `Some(prefix)` when the user passed `--branch-prefix`; the
    /// caller resolves the default (`stack/<author>` or the git
    /// config override) via [`crate::stack_context::resolve_default_branch_prefix`].
    pub branch_prefix: &'a str,
    /// Raw `NAME` argument from the CLI. Normalised inside [`run`].
    pub name: &'a str,
    /// Local branch name override. `None` defaults to the
    /// normalised remote stack name.
    pub local_branch: Option<&'a str>,
    /// Remote name to fetch from — typically `origin`. Comes from
    /// the trunk's first segment.
    pub remote: &'a str,
    pub dry_run: bool,
}

pub async fn run(opts: &Options<'_>) -> Result<Outcome, CliError> {
    // Normalise the stack name. Python:
    //   name = changes.CHANGEID_SUFFIX_RE.sub("", name)
    //   if branch_prefix and name.startswith(f"{branch_prefix}/"):
    //       name = name.removeprefix(f"{branch_prefix}/")
    let mut name = change_id::strip_branch_suffix(opts.name);
    let prefix_with_slash = format!("{}/", opts.branch_prefix);
    if !opts.branch_prefix.is_empty() && name.starts_with(&prefix_with_slash) {
        name = name[prefix_with_slash.len()..].to_string();
    }
    let local_branch = opts.local_branch.unwrap_or(&name).to_string();
    let stack_branch = if opts.branch_prefix.is_empty() {
        name.clone()
    } else {
        format!("{}/{}", opts.branch_prefix, name)
    };

    let remote_changes = remote_changes::get_remote_changes(
        opts.client,
        opts.user,
        opts.repo,
        &stack_branch,
        opts.author,
    )
    .await?;

    let chain = build_chain(&remote_changes, &stack_branch)?;
    if chain.is_empty() {
        return Ok(Outcome::NoStackedPrs);
    }

    let upstream = format!(
        "{remote}/{base}",
        remote = opts.remote,
        base = chain[0].base_ref
    );

    if opts.dry_run {
        return Ok(Outcome::CheckedOut {
            chain,
            created: false,
            local_branch,
            upstream,
        });
    }

    let leaf_head = chain.last().expect("non-empty chain").head_ref.clone();
    let head_ref = format!("{remote}/{leaf_head}", remote = opts.remote);
    run_git(opts.repo_dir, &["fetch", opts.remote, &leaf_head])?;
    run_git(opts.repo_dir, &["checkout", "-b", &local_branch, &head_ref])?;
    run_git(
        opts.repo_dir,
        &["branch", &format!("--set-upstream-to={upstream}")],
    )?;
    Ok(Outcome::CheckedOut {
        chain,
        created: true,
        local_branch,
        upstream,
    })
}

/// Walk the remote-changes graph and return the open-PR chain
/// from root → leaf. Open PRs are linked via `head.ref` →
/// `base.ref`; the root is the one whose `base.ref` doesn't start
/// with the stack branch prefix. Two-root layouts are surfaced as
/// `InvalidState`, matching Python.
fn build_chain(
    remote_changes: &[RemoteChange],
    stack_branch: &str,
) -> Result<Vec<PullSummary>, CliError> {
    // Build a base.ref → pull map of open PRs.
    let mut nodes: std::collections::HashMap<String, &Value> = std::collections::HashMap::new();
    for change in remote_changes {
        let state = change.pull.get("state").and_then(Value::as_str);
        if state != Some("open") {
            continue;
        }
        let base_ref = pull_field(&change.pull, "base", "ref")?;
        nodes.insert(base_ref, &change.pull);
    }
    if nodes.is_empty() {
        return Ok(Vec::new());
    }

    // Find the root — the PR whose base.ref doesn't start with
    // the stack branch (i.e. it's the trunk side).
    let mut root: Option<&Value> = None;
    for pull in nodes.values() {
        let base_ref = pull_field(pull, "base", "ref")?;
        if !base_ref.starts_with(stack_branch) {
            if root.is_some() {
                return Err(CliError::InvalidState(
                    "unexpected stack layout, two root commits found".to_string(),
                ));
            }
            root = Some(*pull);
        }
    }
    let Some(mut current) = root else {
        return Ok(Vec::new());
    };

    // Walk from root to leaf following head.ref → base.ref links.
    let mut chain: Vec<PullSummary> = Vec::new();
    loop {
        chain.push(summary_from(current)?);
        let head_ref = pull_field(current, "head", "ref")?;
        match nodes.get(&head_ref) {
            Some(next) => current = next,
            None => break,
        }
    }
    Ok(chain)
}

fn pull_field(pull: &Value, parent: &str, child: &str) -> Result<String, CliError> {
    pull.get(parent)
        .and_then(|p| p.get(child))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            CliError::Generic(format!("pull request payload missing `{parent}.{child}`"))
        })
}

fn summary_from(pull: &Value) -> Result<PullSummary, CliError> {
    let number = pull
        .get("number")
        .and_then(Value::as_u64)
        .ok_or_else(|| CliError::Generic("pull missing `number`".to_string()))?;
    let title = pull
        .get("title")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| CliError::Generic("pull missing `title`".to_string()))?;
    let html_url = pull
        .get("html_url")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| CliError::Generic("pull missing `html_url`".to_string()))?;
    Ok(PullSummary {
        number,
        title,
        html_url,
        base_ref: pull_field(pull, "base", "ref")?,
        head_ref: pull_field(pull, "head", "ref")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pull(number: u64, base: &str, head: &str) -> RemoteChange {
        RemoteChange {
            change_id: format!("I{number:040}"),
            pull: json!({
                "number": number,
                "title": format!("PR #{number}"),
                "html_url": format!("https://github.com/o/r/pull/{number}"),
                "state": "open",
                "base": {"ref": base},
                "head": {"ref": head},
            }),
        }
    }

    #[test]
    fn builds_chain_from_root_to_leaf() {
        // Stack: main → stack/a/1 → stack/a/2 → stack/a/3
        let stack = "stack/a";
        let changes = vec![
            pull(1, "main", "stack/a/1"),
            pull(2, "stack/a/1", "stack/a/2"),
            pull(3, "stack/a/2", "stack/a/3"),
        ];
        let chain = build_chain(&changes, stack).unwrap();
        let nums: Vec<u64> = chain.iter().map(|p| p.number).collect();
        assert_eq!(nums, [1, 2, 3]);
    }

    #[test]
    fn skips_closed_prs() {
        let stack = "stack/a";
        let changes = vec![pull(1, "main", "stack/a/1"), {
            let mut c = pull(2, "stack/a/1", "stack/a/2");
            c.pull["state"] = json!("closed");
            c
        }];
        let chain = build_chain(&changes, stack).unwrap();
        // Closed PR is skipped — chain is just the root (no leaf).
        let nums: Vec<u64> = chain.iter().map(|p| p.number).collect();
        assert_eq!(nums, [1]);
    }

    #[test]
    fn detects_two_roots() {
        let stack = "stack/a";
        // Both PRs have base.ref outside the stack prefix — two
        // candidate roots, which is malformed.
        let changes = vec![
            pull(1, "main", "stack/a/1"),
            pull(2, "develop", "stack/a/2"),
        ];
        let err = build_chain(&changes, stack).unwrap_err();
        match err {
            CliError::InvalidState(msg) => assert!(msg.contains("two root commits"), "got: {msg}"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn no_open_prs_returns_empty() {
        let stack = "stack/a";
        let mut c = pull(1, "main", "stack/a/1");
        c.pull["state"] = json!("closed");
        let chain = build_chain(&[c], stack).unwrap();
        assert!(chain.is_empty());
    }

    #[tokio::test]
    async fn run_no_stacked_prs_returns_no_stacked_prs() {
        use mergify_core::ApiFlavor;
        use url::Url;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/issues"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"items": []})),
            )
            .mount(&server)
            .await;

        let client = HttpClient::new(
            Url::parse(&server.uri()).unwrap(),
            "tok".to_string(),
            ApiFlavor::GitHub,
        )
        .unwrap();

        let outcome = run(&Options {
            repo_dir: None,
            client: &client,
            user: "user",
            repo: "repo",
            author: "author",
            branch_prefix: "stack/author",
            name: "my-branch",
            local_branch: None,
            remote: "origin",
            dry_run: true,
        })
        .await
        .unwrap();
        assert!(matches!(outcome, Outcome::NoStackedPrs));
    }

    #[tokio::test]
    async fn run_dry_run_returns_chain_without_touching_git() {
        use mergify_core::ApiFlavor;
        use url::Url;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/issues"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{"number": 1}, {"number": 2}],
            })))
            .mount(&server)
            .await;
        // Head refs use the new-format `<slug>--<hex8>` shape so
        // `extract_from_branch_segment` accepts them and the
        // remote_changes pipeline doesn't filter them out.
        Mock::given(method("GET"))
            .and(path("/repos/user/repo/pulls/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 1,
                "title": "feat: A",
                "html_url": "https://github.com/user/repo/pull/1",
                "state": "open",
                "base": {"ref": "main"},
                "head": {"ref": "stack/author/my-branch/feat-a--aaaaaaaa"},
                "merged_at": null,
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/user/repo/pulls/2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 2,
                "title": "feat: B",
                "html_url": "https://github.com/user/repo/pull/2",
                "state": "open",
                "base": {"ref": "stack/author/my-branch/feat-a--aaaaaaaa"},
                "head": {"ref": "stack/author/my-branch/feat-b--bbbbbbbb"},
                "merged_at": null,
            })))
            .mount(&server)
            .await;

        let client = HttpClient::new(
            Url::parse(&server.uri()).unwrap(),
            "tok".to_string(),
            ApiFlavor::GitHub,
        )
        .unwrap();

        let outcome = run(&Options {
            repo_dir: None,
            client: &client,
            user: "user",
            repo: "repo",
            author: "author",
            branch_prefix: "stack/author",
            name: "my-branch",
            local_branch: None,
            remote: "origin",
            dry_run: true,
        })
        .await
        .unwrap();
        match outcome {
            Outcome::CheckedOut {
                chain,
                created,
                local_branch,
                upstream,
            } => {
                let nums: Vec<u64> = chain.iter().map(|p| p.number).collect();
                assert_eq!(nums, [1, 2]);
                assert!(!created);
                assert_eq!(local_branch, "my-branch");
                assert_eq!(upstream, "origin/main");
            }
            Outcome::NoStackedPrs => panic!("unexpected NoStackedPrs"),
        }
    }

    #[tokio::test]
    async fn run_strips_changeid_suffix_and_branch_prefix_from_name() {
        // The user pastes a leaf branch name with both the
        // `<prefix>/` prefix and a trailing `/I<40hex>` suffix.
        // We expect the search to be issued against the stem
        // `stack/author/my-branch`.
        use mergify_core::ApiFlavor;
        use url::Url;
        use wiremock::matchers::{method, path, query_param_contains};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/issues"))
            .and(query_param_contains("q", "head:stack/author/my-branch/"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"items": []})),
            )
            .mount(&server)
            .await;

        let client = HttpClient::new(
            Url::parse(&server.uri()).unwrap(),
            "tok".to_string(),
            ApiFlavor::GitHub,
        )
        .unwrap();

        let outcome = run(&Options {
            repo_dir: None,
            client: &client,
            user: "user",
            repo: "repo",
            author: "author",
            branch_prefix: "stack/author",
            name: "stack/author/my-branch/Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            local_branch: None,
            remote: "origin",
            dry_run: true,
        })
        .await
        .unwrap();
        assert!(matches!(outcome, Outcome::NoStackedPrs));
    }
}
