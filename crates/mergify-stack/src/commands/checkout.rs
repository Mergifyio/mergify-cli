//! `mergify stack checkout <BRANCH|PR_URL>` — fetch a stack of
//! pull requests from GitHub and create a local branch tracking
//! the leaf head.
//!
//! The target is whatever identifies the stack on GitHub:
//!
//! - a **stack branch**, exactly as it exists on the remote
//!   (`stack/jd/my-feature`, `mystacks/foo`, `feature-x`, …). A
//!   leaf ref pasted verbatim works too — the trailing Change-Id
//!   segment gets stripped back to the stack stem.
//! - a **pull-request URL**, from any position in the stack. The
//!   PR's `head.ref` yields the stack branch, so a URL grabbed
//!   from the middle of a stack still checks out the whole thing.
//!
//! Nothing is derived from a user login. Other people's stacks,
//! and stacks under a branch prefix with no author in it, are as
//! reachable as your own.
//!
//! The flow:
//!
//! 1. Resolve the target into a stack branch (+ `owner/repo`,
//!    which a PR URL carries).
//! 2. Search GitHub for the stack's PRs (via
//!    [`crate::remote_changes::get_remote_changes`], with no
//!    `author:` filter — a branch name is unique within a repo).
//! 3. Link open PRs into a single chain via their `head.ref` →
//!    `base.ref` pointers, find the root (the PR whose `base.ref`
//!    is *outside* the stack — i.e. doesn't start with the stack
//!    branch prefix), walk up to the leaf.
//! 4. Print the chain. When not `--dry-run`, `git fetch` the leaf
//!    head, `git checkout -b <local>` on it, set upstream
//!    tracking to the root's base.

use std::path::Path;

use crate::git::run_git_silent as run_git;

use mergify_core::CliError;
use mergify_core::HttpClient;
use serde_json::Value;
use url::Url;

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
        /// The remote stack branch the target resolved to. Worth
        /// echoing back: when the target was a PR URL, the user
        /// never typed it.
        stack_branch: String,
        /// Whether the stack sits under this machine's stack
        /// branch prefix. When it doesn't, the other stack
        /// commands — which still scope themselves to the local
        /// user's own prefix and login — won't reach these pull
        /// requests from the branch just created.
        under_local_prefix: bool,
    },
    NoStackedPrs,
}

/// What the user pointed `stack checkout` at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A stack branch, already stripped of any trailing Change-Id
    /// segment.
    Branch(String),
    /// A pull request, identified by the `<owner>/<repo>/pull/<n>`
    /// part of its URL. The host is deliberately ignored — requests
    /// go to the configured API endpoint either way, and demanding
    /// a match would only reject working input on the GHES setups
    /// where the web host and the API host differ.
    Pull {
        owner: String,
        repo: String,
        number: u64,
    },
}

/// Classify the raw CLI argument.
///
/// An `http(s)://` target has to be a pull request URL — letting a
/// wrong one through as a "branch" would only produce a baffling
/// "no stacked PRs" further down. A schemeless target is read as a
/// pull request URL too when it carries a host-looking first
/// segment (`github.com/o/r/pull/12`, what a copied chat link
/// usually looks like); anything else is a branch name.
pub fn parse_target(raw: &str) -> Result<Target, CliError> {
    let invalid = || {
        CliError::InvalidState(format!(
            "`{raw}` is not a pull request URL — expected `http(s)://<host>/<owner>/<repo>/pull/<number>`",
        ))
    };

    if raw.starts_with("https://") || raw.starts_with("http://") {
        let url = Url::parse(raw).map_err(|_| invalid())?;
        let segments: Vec<&str> = url
            .path_segments()
            .map(Iterator::collect)
            .unwrap_or_default();
        // A scheme is an unambiguous "this is a URL", so a shape
        // that doesn't match is an error rather than a branch.
        return match_pull(&segments).ok_or_else(invalid);
    }

    // Trailing slashes come from shell completion on a remote ref;
    // left in place they'd survive into an empty final segment.
    let raw = raw.trim_end_matches('/');
    // `<host>/<owner>/<repo>/pull/<n>` — the leading dot-bearing
    // segment is what separates a pasted link from a branch that
    // merely contains a `pull` segment.
    if let [host, rest @ ..] = raw.split('/').collect::<Vec<_>>().as_slice()
        && host.contains('.')
        && let Some(target) = match_pull(rest)
    {
        return Ok(target);
    }
    Ok(Target::Branch(change_id::strip_branch_suffix(raw)))
}

/// Match `<owner>/<repo>/pull/<number>` at the head of `segments`.
/// Trailing segments (`/files`, `/commits/<sha>`, …) and any
/// fragment are what a browser address bar hands you, so they are
/// matched past and ignored.
fn match_pull(segments: &[&str]) -> Option<Target> {
    match segments {
        [owner, repo, "pull", number, ..] if !owner.is_empty() && !repo.is_empty() => {
            Some(Target::Pull {
                owner: (*owner).to_string(),
                repo: (*repo).to_string(),
                number: number.parse().ok()?,
            })
        }
        _ => None,
    }
}

pub struct Options<'a> {
    pub repo_dir: Option<&'a Path>,
    pub client: &'a HttpClient,
    /// `--repository` override, in `owner/repo` form. `None` reads
    /// the remote's URL. Consulted only for a branch target — a PR
    /// URL carries its own owner and repo, and resolving the local
    /// remote for it could fail on a repository that has nothing to
    /// do with the target.
    pub repository: Option<&'a str>,
    /// Raw target argument from the CLI — a stack branch or a pull
    /// request URL. Classified by [`parse_target`].
    pub target: &'a str,
    /// Local branch name override. `None` derives one from the
    /// stack branch — see [`derive_local_branch`].
    pub local_branch: Option<&'a str>,
    /// This machine's stack branch prefix, as the other stack
    /// commands would compute it. Used only to name the local
    /// branch, and to tell a stack of your own from someone
    /// else's; `None` when the caller couldn't determine one.
    pub local_stack_prefix: Option<&'a str>,
    /// Remote name to fetch from — typically `origin`. Comes from
    /// the trunk's first segment.
    pub remote: &'a str,
    pub dry_run: bool,
}

pub async fn run(opts: &Options<'_>) -> Result<Outcome, CliError> {
    let (owner, repo, stack_branch) = resolve_stack_branch(opts).await?;

    let below_prefix = opts
        .local_stack_prefix
        .and_then(|prefix| stack_branch.strip_prefix(&format!("{prefix}/")));
    let under_local_prefix = below_prefix.is_some();
    let local_branch = match opts.local_branch {
        Some(branch) => branch.to_string(),
        None => derive_local_branch(&stack_branch, below_prefix),
    };

    let remote_changes =
        remote_changes::get_remote_changes(opts.client, &owner, &repo, &stack_branch, None).await?;

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
            stack_branch,
            under_local_prefix,
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
        stack_branch,
        under_local_prefix,
    })
}

/// Name the local branch for a stack branch the user didn't name
/// one for.
///
/// `below_prefix` is what's left of the stack branch after this
/// machine's stack prefix — `Some` only when the stack sits under
/// it. That remainder is the branch the stack was pushed from, and
/// restoring it exactly is what lets a later `stack push` find the
/// same stack instead of opening a duplicate set of pull requests:
/// push recomposes `<prefix>/<branch>`, so a multi-segment name
/// like `feature/login` has to survive whole.
///
/// Without that (someone else's stack, or no prefix to compare
/// against) there's no way to tell where the prefix ends, and the
/// last segment is the closest thing to the name a human chose.
fn derive_local_branch(stack_branch: &str, below_prefix: Option<&str>) -> String {
    below_prefix.map_or_else(
        || {
            stack_branch
                .rsplit('/')
                .next()
                .unwrap_or(stack_branch)
                .to_string()
        },
        ToString::to_string,
    )
}

/// Turn the raw target into `(owner, repo, stack_branch)`. A PR
/// URL costs one extra request and names its own repository; a
/// branch target resolves the local one, which is why that lookup
/// happens here and not before the target is classified.
async fn resolve_stack_branch(opts: &Options<'_>) -> Result<(String, String, String), CliError> {
    let (owner, repo, stack_branch) = match parse_target(opts.target)? {
        Target::Branch(branch) => {
            let slug =
                crate::stack_context::resolve_repo(opts.repo_dir, opts.repository, opts.remote)?;
            (slug.owner, slug.repo, branch)
        }
        Target::Pull {
            owner,
            repo,
            number,
        } => {
            let pull: Value = opts
                .client
                .get(&format!("/repos/{owner}/{repo}/pulls/{number}"))
                .await?;
            let head_ref = pull_field(&pull, "head", "ref")?;
            let stack_branch = change_id::strip_branch_suffix(&head_ref);
            (owner, repo, stack_branch)
        }
    };
    if stack_branch.is_empty() {
        return Err(CliError::InvalidState(
            "empty stack branch — pass the stack's branch name or a pull request URL".to_string(),
        ));
    }
    Ok((owner, repo, stack_branch))
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
            repository: Some("user/repo"),
            local_stack_prefix: None,
            target: "stack/author/my-branch",
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
            repository: Some("user/repo"),
            local_stack_prefix: None,
            target: "stack/author/my-branch",
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
                ..
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
    async fn run_strips_changeid_suffix_from_a_pasted_leaf_ref() {
        // The user pastes a leaf branch ref verbatim, current
        // `<slug>--<8 hex>` naming included. The search has to be
        // issued against the stack stem, not the leaf.
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
            repository: Some("user/repo"),
            local_stack_prefix: None,
            target: "stack/author/my-branch/feat-b--bbbbbbbb",
            local_branch: None,
            remote: "origin",
            dry_run: true,
        })
        .await
        .unwrap();
        assert!(matches!(outcome, Outcome::NoStackedPrs));
    }

    #[test]
    fn parse_target_reads_owner_repo_and_number_from_a_pull_url() {
        assert_eq!(
            parse_target("https://github.com/Mergifyio/mergify-cli/pull/1774").unwrap(),
            Target::Pull {
                owner: "Mergifyio".to_string(),
                repo: "mergify-cli".to_string(),
                number: 1774,
            },
        );
    }

    #[test]
    fn parse_target_accepts_a_pull_url_with_tab_suffix_and_fragment() {
        // What you get from a browser address bar mid-review.
        assert_eq!(
            parse_target("https://github.com/o/r/pull/12/files#diff-abc").unwrap(),
            Target::Pull {
                owner: "o".to_string(),
                repo: "r".to_string(),
                number: 12,
            },
        );
    }

    #[test]
    fn parse_target_accepts_a_pull_url_on_an_enterprise_host() {
        assert_eq!(
            parse_target("https://ghe.example.com/o/r/pull/7").unwrap(),
            Target::Pull {
                owner: "o".to_string(),
                repo: "r".to_string(),
                number: 7,
            },
        );
    }

    #[test]
    fn parse_target_treats_anything_else_as_a_branch_and_strips_the_changeid() {
        assert_eq!(
            parse_target("stack/author/my-branch/feat-b--bbbbbbbb").unwrap(),
            Target::Branch("stack/author/my-branch".to_string()),
        );
        assert_eq!(
            parse_target("my-feature").unwrap(),
            Target::Branch("my-feature".to_string()),
        );
    }

    #[test]
    fn parse_target_accepts_a_pull_url_pasted_without_its_scheme() {
        // Copying a PR link out of a chat message routinely loses
        // the scheme. Treating it as a branch would search for a
        // stack that cannot exist and report "no stacked PRs".
        assert_eq!(
            parse_target("github.com/Mergifyio/mergify-cli/pull/1774").unwrap(),
            Target::Pull {
                owner: "Mergifyio".to_string(),
                repo: "mergify-cli".to_string(),
                number: 1774,
            },
        );
    }

    #[test]
    fn parse_target_keeps_a_branch_whose_segments_merely_resemble_a_pull_url() {
        // No host-looking first segment, so this is a branch that
        // happens to contain `pull`, not a URL.
        assert_eq!(
            parse_target("team/repo/pull/12").unwrap(),
            Target::Branch("team/repo/pull/12".to_string()),
        );
    }

    #[test]
    fn parse_target_trims_trailing_slashes_from_a_branch() {
        // Shell tab-completion on a remote ref adds the slash. It
        // would otherwise survive into an empty last segment and
        // make `git checkout -b ''` fail with a git-level error.
        assert_eq!(
            parse_target("stack/author/my-branch/").unwrap(),
            Target::Branch("stack/author/my-branch".to_string()),
        );
    }

    #[test]
    fn parse_target_rejects_a_url_that_is_not_a_pull_request() {
        // Treating this as a branch name would send the user into
        // a confusing "no stacked PRs" instead of naming the
        // mistake.
        let err = parse_target("https://github.com/o/r/issues/12").unwrap_err();
        match err {
            CliError::InvalidState(msg) => assert!(msg.contains("pull request URL"), "got: {msg}"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_checks_out_the_whole_stack_from_a_middle_pull_url() {
        // A PR URL from the middle of a three-PR stack must yield
        // the full root→leaf chain, not just that PR and below.
        use mergify_core::ApiFlavor;
        use url::Url;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn pr_body(number: u64, base: &str, head: &str) -> serde_json::Value {
            serde_json::json!({
                "number": number,
                "title": format!("feat: {number}"),
                "html_url": format!("https://github.com/user/repo/pull/{number}"),
                "state": "open",
                "base": {"ref": base},
                "head": {"ref": head},
                "merged_at": null,
            })
        }

        let stack = "stack/author/my-branch";
        let heads = [
            format!("{stack}/feat-a--aaaaaaaa"),
            format!("{stack}/feat-b--bbbbbbbb"),
            format!("{stack}/feat-c--cccccccc"),
        ];
        let bodies = [
            pr_body(1, "main", &heads[0]),
            pr_body(2, &heads[0], &heads[1]),
            pr_body(3, &heads[1], &heads[2]),
        ];

        let server = MockServer::start().await;
        // No `author:` qualifier — checkout reaches anyone's stack.
        Mock::given(method("GET"))
            .and(path("/search/issues"))
            .and(query_param(
                "q",
                "repo:user/repo is:pull-request head:stack/author/my-branch/",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{"number": 1}, {"number": 2}, {"number": 3}],
            })))
            .mount(&server)
            .await;
        for (i, body) in bodies.iter().enumerate() {
            Mock::given(method("GET"))
                .and(path(format!("/repos/user/repo/pulls/{}", i + 1)))
                .respond_with(ResponseTemplate::new(200).set_body_json(body.clone()))
                .mount(&server)
                .await;
        }

        let client = HttpClient::new(
            Url::parse(&server.uri()).unwrap(),
            "tok".to_string(),
            ApiFlavor::GitHub,
        )
        .unwrap();

        let outcome = run(&Options {
            repo_dir: None,
            client: &client,
            // Deliberately wrong — the PR URL's owner/repo wins.
            repository: Some("somebody-else/other-repo"),
            local_stack_prefix: None,
            target: "https://github.com/user/repo/pull/2",
            local_branch: None,
            remote: "origin",
            dry_run: true,
        })
        .await
        .unwrap();

        match outcome {
            Outcome::CheckedOut {
                chain,
                local_branch,
                upstream,
                stack_branch,
                ..
            } => {
                let nums: Vec<u64> = chain.iter().map(|p| p.number).collect();
                assert_eq!(nums, [1, 2, 3]);
                assert_eq!(stack_branch, stack);
                // Local branch defaults to the stack's last segment.
                assert_eq!(local_branch, "my-branch");
                assert_eq!(upstream, "origin/main");
            }
            Outcome::NoStackedPrs => panic!("unexpected NoStackedPrs"),
        }
    }

    /// Mount a two-PR stack under `stack_branch` and return the
    /// client pointed at it. The server has to outlive the call,
    /// so it comes back with the client.
    async fn stack_server(stack_branch: &str) -> (wiremock::MockServer, HttpClient) {
        use mergify_core::ApiFlavor;
        use url::Url;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let heads = [
            format!("{stack_branch}/feat-a--aaaaaaaa"),
            format!("{stack_branch}/feat-b--bbbbbbbb"),
        ];
        let bodies = [
            serde_json::json!({
                "number": 1, "title": "feat: A", "state": "open",
                "html_url": "https://github.com/user/repo/pull/1",
                "base": {"ref": "main"}, "head": {"ref": heads[0]}, "merged_at": null,
            }),
            serde_json::json!({
                "number": 2, "title": "feat: B", "state": "open",
                "html_url": "https://github.com/user/repo/pull/2",
                "base": {"ref": heads[0]}, "head": {"ref": heads[1]}, "merged_at": null,
            }),
        ];

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/issues"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{"number": 1}, {"number": 2}],
            })))
            .mount(&server)
            .await;
        for (i, body) in bodies.iter().enumerate() {
            Mock::given(method("GET"))
                .and(path(format!("/repos/user/repo/pulls/{}", i + 1)))
                .respond_with(ResponseTemplate::new(200).set_body_json(body.clone()))
                .mount(&server)
                .await;
        }

        let client = HttpClient::new(
            Url::parse(&server.uri()).unwrap(),
            "tok".to_string(),
            ApiFlavor::GitHub,
        )
        .unwrap();
        (server, client)
    }

    #[tokio::test]
    async fn run_keeps_the_whole_branch_name_below_the_local_stack_prefix() {
        // `stack new feature/login` pushes to
        // `<prefix>/feature/login/…`. Checking that back out has to
        // restore `feature/login` — a local branch of just `login`
        // would make the next `stack push` compute
        // `<prefix>/login`, match nothing, and open a duplicate set
        // of pull requests for the same commits.
        let (_server, client) = stack_server("devs/jd/feature/login").await;

        let outcome = run(&Options {
            repo_dir: None,
            client: &client,
            repository: Some("user/repo"),
            local_stack_prefix: Some("devs/jd"),
            target: "devs/jd/feature/login",
            local_branch: None,
            remote: "origin",
            dry_run: true,
        })
        .await
        .unwrap();

        match outcome {
            Outcome::CheckedOut {
                local_branch,
                under_local_prefix,
                ..
            } => {
                assert_eq!(local_branch, "feature/login");
                assert!(under_local_prefix);
            }
            Outcome::NoStackedPrs => panic!("unexpected NoStackedPrs"),
        }
    }

    #[tokio::test]
    async fn run_flags_a_stack_that_is_not_under_the_local_prefix() {
        // Someone else's stack: there's no prefix to strip, so the
        // last segment is the best local name — and `stack push`
        // from it would not reach their pull requests, which the
        // caller needs to be able to say.
        let (_server, client) = stack_server("devs/alice/feat").await;

        let outcome = run(&Options {
            repo_dir: None,
            client: &client,
            repository: Some("user/repo"),
            local_stack_prefix: Some("devs/jd"),
            target: "devs/alice/feat",
            local_branch: None,
            remote: "origin",
            dry_run: true,
        })
        .await
        .unwrap();

        match outcome {
            Outcome::CheckedOut {
                local_branch,
                under_local_prefix,
                ..
            } => {
                assert_eq!(local_branch, "feat");
                assert!(!under_local_prefix);
            }
            Outcome::NoStackedPrs => panic!("unexpected NoStackedPrs"),
        }
    }

    #[tokio::test]
    async fn run_does_not_resolve_the_repository_for_a_pull_url_target() {
        // A PR URL carries its own owner/repo, so an unparseable
        // `--repository` must not be consulted at all.
        let (_server, client) = stack_server("stack/author/my-branch").await;

        let outcome = run(&Options {
            repo_dir: None,
            client: &client,
            repository: Some("not-a-slug"),
            local_stack_prefix: None,
            target: "https://github.com/user/repo/pull/2",
            local_branch: None,
            remote: "origin",
            dry_run: true,
        })
        .await
        .unwrap();

        match outcome {
            Outcome::CheckedOut { stack_branch, .. } => {
                assert_eq!(stack_branch, "stack/author/my-branch");
            }
            Outcome::NoStackedPrs => panic!("unexpected NoStackedPrs"),
        }
    }
}
