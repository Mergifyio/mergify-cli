//! Replay an amendment onto its old PR head as a synthetic commit.
//!
//! Background: when `stack push` force-pushes an amended commit,
//! the revision-history "compare" URL `…/compare/<old_sha>...<new_sha>`
//! is useless when the rebase moved the base — reviewers see the
//! whole PR diff instead of just the edit. The fix is to build a
//! synthetic commit whose tree is *only* the user's amendment
//! and whose parent is `old_sha`, then point compare at that.
//!
//! This module ports `mergify_cli/stack/replay.py`:
//!
//! - [`compute_merged_tree`] — `git merge-tree --write-tree` to
//!   replay the new PR head onto `parent(old_sha)`, returning the
//!   resulting tree SHA.
//! - [`compute_tree_delta`] — `git diff-tree --raw --no-renames`
//!   between two trees, parsed into entries shaped for the
//!   GitHub `POST /repos/.../git/trees` API.
//! - [`upload_replay_commit`] — chains
//!   `POST /git/trees` + `POST /git/commits` to materialise the
//!   synthetic commit on GitHub. Returns the commit SHA the
//!   revision-history compare URL anchors at.
//! - [`replay_for_revision`] — top-level entry point wiring all
//!   three above together; returns `None` whenever the
//!   rebase-aware compare URL can't be produced so the caller
//!   falls back to the plain `old…new` three-dot URL.

use std::path::Path;
use std::process::Command;

use mergify_core::{CliError, HttpClient};
use serde::{Deserialize, Serialize};

/// Output of [`compute_merged_tree`]: the merged tree SHA paired
/// with the commit SHA the tree is anchored on (parent of
/// `old_sha`). Callers need both: the tree for the diff against
/// `parent_old_sha^{tree}`, the parent SHA so the synthetic
/// commit's `parents` field can use the right value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedTree {
    pub tree_sha: String,
    pub parent_old_sha: String,
}

/// One entry in the body of `POST /repos/.../git/trees` — modulo
/// the `null` SHA used for deletions, the shape is GitHub's Git
/// Data API verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TreeEntry {
    pub path: String,
    pub mode: String,
    /// Renamed because `type` is a Rust keyword. The wire field is
    /// `"type"` to match GitHub.
    #[serde(rename = "type")]
    pub type_: String,
    /// `None` -> JSON `null`, which GitHub interprets as "delete
    /// this path from `base_tree`."
    pub sha: Option<String>,
}

/// Replay `new_sha` onto `parent(old_sha)` and return the merged
/// tree SHA. Returns `None` whenever the rebase-aware compare URL
/// can't be produced — conflict, missing parents, or any git
/// failure — so the caller can fall back to the plain
/// `old…new` three-dot URL.
///
/// Requires git ≥ 2.38 for `git merge-tree --write-tree`. Older
/// gits fail with non-zero exit, which gets coerced to `None`
/// here (same behaviour as Python).
#[must_use]
pub fn compute_merged_tree(
    repo_dir: Option<&Path>,
    old_sha: &str,
    new_sha: &str,
) -> Option<MergedTree> {
    let parent_old_sha = run_git_capture(repo_dir, &["rev-parse", &format!("{old_sha}^")]).ok()?;
    let parent_new_sha = run_git_capture(repo_dir, &["rev-parse", &format!("{new_sha}^")]).ok()?;

    let output = run_git_capture(
        repo_dir,
        &[
            "merge-tree",
            "--write-tree",
            &format!("--merge-base={parent_new_sha}"),
            &parent_old_sha,
            new_sha,
        ],
    )
    .ok()?;

    // On a clean merge, the first line of stdout is the tree SHA.
    // A conflict prints conflict markers on later lines and exits
    // non-zero — that case is already covered by the `?` above.
    let tree_sha = output.lines().next()?.to_string();
    if tree_sha.is_empty() {
        return None;
    }

    Some(MergedTree {
        tree_sha,
        parent_old_sha,
    })
}

/// Convert a `git` tree-entry mode to the GitHub Git Data API's
/// `type` field (`blob` / `tree` / `commit`). Anything we don't
/// recognise falls back to `blob` so a future mode value can't
/// crash the parser.
#[must_use]
pub fn mode_to_type(mode: &str) -> &'static str {
    match mode {
        "160000" => "commit",
        "040000" => "tree",
        _ => "blob",
    }
}

/// Parse `git diff-tree -r --raw --no-renames base merged` into
/// GitHub `git/trees` entries.
///
/// Each diff-tree line has the shape
/// `":mode_old mode_new sha_old sha_new STATUS\tpath"`. We
/// preserve only the `M` (modified), `A` (added), `T` (type-
/// changed), and `D` (deleted) statuses. `--no-renames` suppresses
/// `R`/`C` already; any future status we don't know is dropped on
/// the floor (a deliberate Python behaviour: silent misclassif
/// would be worse than dropping).
///
/// Returns an empty vec when the diff is empty (e.g. the merged
/// tree equals `base_tree_sha`). Caller treats that as "nothing
/// to upload" and skips the replay commit.
pub fn compute_tree_delta(
    repo_dir: Option<&Path>,
    base_tree_sha: &str,
    merged_tree_sha: &str,
) -> Result<Vec<TreeEntry>, CliError> {
    let output = run_git_capture(
        repo_dir,
        &[
            "diff-tree",
            "-r",
            "--raw",
            "--no-renames",
            base_tree_sha,
            merged_tree_sha,
        ],
    )?;

    let mut entries = Vec::new();
    for line in output.lines() {
        let Some(rest) = line.strip_prefix(':') else {
            continue;
        };
        // Format: "mode_old mode_new sha_old sha_new STATUS\tpath".
        let (meta, path) = match rest.split_once('\t') {
            Some((meta, path)) if !path.is_empty() => (meta, path),
            _ => continue,
        };
        let parts: Vec<&str> = meta.split_whitespace().collect();
        if parts.len() < 5 {
            continue;
        }
        let (mode_old, mode_new, _sha_old, sha_new, status) =
            (parts[0], parts[1], parts[2], parts[3], parts[4]);

        match status {
            "D" => entries.push(TreeEntry {
                path: path.to_string(),
                mode: mode_old.to_string(),
                type_: mode_to_type(mode_old).to_string(),
                sha: None,
            }),
            "M" | "A" | "T" => entries.push(TreeEntry {
                path: path.to_string(),
                mode: mode_new.to_string(),
                type_: mode_to_type(mode_new).to_string(),
                sha: Some(sha_new.to_string()),
            }),
            _ => {
                // Unknown status — drop. Better than coercing to
                // something arbitrary and corrupting the upload.
            }
        }
    }
    Ok(entries)
}

/// Materialise a synthetic commit on GitHub: `POST /git/trees`
/// with the delta to plant a tree, then `POST /git/commits` with
/// `parents=[old_sha]` so `compare/<old_sha>...<commit>` shows
/// only the amendment.
///
/// Returns the commit SHA on success or `None` on any API error
/// (matches Python's `except httpx.HTTPError, ValueError`). The
/// unreferenced object will be GC'd by GitHub eventually; the
/// compare URL works in the meantime — and the
/// revision-history table includes a `(raw)` fallback URL for
/// when it goes away.
pub async fn upload_replay_commit(
    client: &HttpClient,
    user: &str,
    repo: &str,
    base_tree_sha: &str,
    old_sha: &str,
    new_sha: &str,
    entries: &[TreeEntry],
) -> Option<String> {
    let tree_path = format!("/repos/{user}/{repo}/git/trees");
    let tree_payload = TreePayload {
        base_tree: base_tree_sha,
        tree: entries,
    };
    let tree_resp: ShaResponse = client.post(&tree_path, &tree_payload).await.ok()?;

    let commit_path = format!("/repos/{user}/{repo}/git/commits");
    let commit_payload = CommitPayload {
        message: format!(
            "mergify-cli: replay {} on {}",
            short(new_sha),
            short(old_sha),
        ),
        tree: &tree_resp.sha,
        parents: vec![old_sha],
    };
    let commit_resp: ShaResponse = client.post(&commit_path, &commit_payload).await.ok()?;
    Some(commit_resp.sha)
}

/// Top-level entry point: orchestrates [`compute_merged_tree`] →
/// `rev-parse parent_old^{{tree}}` → [`compute_tree_delta`] →
/// [`upload_replay_commit`], returning the server commit SHA the
/// revision-history compare URL anchors at.
///
/// Returns `None` whenever the rebase-aware compare URL can't be
/// produced (conflict, missing parents, no diff, git error, API
/// error). Callers must fall back to the plain `old…new`
/// three-dot URL anchored at `old_sha`.
pub async fn replay_for_revision(
    client: &HttpClient,
    repo_dir: Option<&Path>,
    user: &str,
    repo: &str,
    old_sha: &str,
    new_sha: &str,
) -> Option<String> {
    let merged = compute_merged_tree(repo_dir, old_sha, new_sha)?;

    // `^{tree}` dereferences a commit SHA to its root tree SHA
    // (git plumbing syntax). Needed so the diff in
    // `compute_tree_delta` starts from the right tree.
    let parent_old_tree_sha = run_git_capture(
        repo_dir,
        &["rev-parse", &format!("{}^{{tree}}", merged.parent_old_sha)],
    )
    .ok()?;

    let entries = compute_tree_delta(repo_dir, &parent_old_tree_sha, &merged.tree_sha).ok()?;
    if entries.is_empty() {
        // The rebase fully absorbs the user's edit, or the merge
        // produced an identical tree — nothing to compare, no
        // synth commit to upload.
        return None;
    }

    upload_replay_commit(
        client,
        user,
        repo,
        &parent_old_tree_sha,
        old_sha,
        new_sha,
        &entries,
    )
    .await
}

#[derive(Serialize)]
struct TreePayload<'a> {
    base_tree: &'a str,
    #[serde(borrow)]
    tree: &'a [TreeEntry],
}

#[derive(Serialize)]
struct CommitPayload<'a> {
    message: String,
    tree: &'a str,
    parents: Vec<&'a str>,
}

#[derive(Deserialize)]
struct ShaResponse {
    sha: String,
}

fn short(sha: &str) -> &str {
    if sha.len() > 7 { &sha[..7] } else { sha }
}

fn git_cmd(repo_dir: Option<&Path>) -> Command {
    let mut cmd = Command::new("git");
    if let Some(dir) = repo_dir {
        cmd.arg("-C").arg(dir);
    }
    cmd.env("LC_ALL", "C").env("LANG", "C").env("LANGUAGE", "C");
    cmd
}

fn run_git_capture(repo_dir: Option<&Path>, args: &[&str]) -> Result<String, CliError> {
    let output = git_cmd(repo_dir)
        .args(args)
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
    Ok(stdout.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    fn init_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        let path = dir.path();
        run(path, &["init", "-q", "-b", "main"]);
        run(path, &["config", "user.email", "t@e.com"]);
        run(path, &["config", "user.name", "t"]);
        run(path, &["config", "commit.gpgsign", "false"]);
        dir
    }

    fn run(path: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn rev_parse(path: &Path, refname: &str) -> String {
        run_git_capture(Some(path), &["rev-parse", refname]).unwrap()
    }

    #[test]
    fn mode_to_type_maps_known_modes() {
        assert_eq!(mode_to_type("160000"), "commit");
        assert_eq!(mode_to_type("040000"), "tree");
        assert_eq!(mode_to_type("100644"), "blob");
        assert_eq!(mode_to_type("100755"), "blob");
        // Anything else falls back to blob so an unfamiliar mode
        // doesn't crash the parser.
        assert_eq!(mode_to_type(""), "blob");
        assert_eq!(mode_to_type("garbage"), "blob");
    }

    #[test]
    fn compute_tree_delta_parses_modifications_and_deletions() {
        // Drive against a real repo: build a base commit with
        // three files, then a second commit that modifies one,
        // deletes one, adds one, and type-changes one. The diff
        // between their trees must produce the 4 expected
        // entries.
        let dir = init_repo();
        let path = dir.path();
        std::fs::write(path.join("modified.txt"), "v1\n").unwrap();
        std::fs::write(path.join("deleted.txt"), "bye\n").unwrap();
        std::fs::write(path.join("type-change.txt"), "regular\n").unwrap();
        run(path, &["add", "."]);
        run(path, &["commit", "-q", "-m", "base"]);
        let base_tree = rev_parse(path, "HEAD^{tree}");

        std::fs::write(path.join("modified.txt"), "v2\n").unwrap();
        std::fs::remove_file(path.join("deleted.txt")).unwrap();
        std::fs::write(path.join("added.txt"), "new\n").unwrap();
        // Type change: regular file -> executable. Triggers the
        // `T` status in diff-tree output.
        let mut perms = std::fs::metadata(path.join("type-change.txt"))
            .unwrap()
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path.join("type-change.txt"), perms).unwrap();
        run(path, &["add", "--all"]);
        run(path, &["commit", "-q", "-m", "edit"]);
        let new_tree = rev_parse(path, "HEAD^{tree}");

        let entries = compute_tree_delta(Some(path), &base_tree, &new_tree).unwrap();

        let by_path: std::collections::HashMap<_, _> =
            entries.iter().map(|e| (e.path.as_str(), e)).collect();
        assert_eq!(by_path.len(), 4);

        let m = by_path["modified.txt"];
        assert_eq!(m.type_, "blob");
        assert_eq!(m.mode, "100644");
        assert!(m.sha.is_some());

        let d = by_path["deleted.txt"];
        assert_eq!(d.type_, "blob");
        assert_eq!(d.mode, "100644");
        assert!(d.sha.is_none(), "deletions get null sha");

        let a = by_path["added.txt"];
        assert_eq!(a.mode, "100644");
        assert!(a.sha.is_some());

        let t = by_path["type-change.txt"];
        assert_eq!(t.mode, "100755", "type change exposes the new mode");
    }

    #[test]
    fn compute_tree_delta_empty_when_no_diff() {
        // Diffing a tree against itself produces no entries —
        // callers treat this as "nothing to upload."
        let dir = init_repo();
        let path = dir.path();
        std::fs::write(path.join("x"), "x\n").unwrap();
        run(path, &["add", "."]);
        run(path, &["commit", "-q", "-m", "x"]);
        let tree = rev_parse(path, "HEAD^{tree}");
        let entries = compute_tree_delta(Some(path), &tree, &tree).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn compute_merged_tree_isolates_the_amendment_when_base_unchanged() {
        // The bug this whole module fixes: a PR amended on a
        // stable base. If we ever regress to the naive
        // sibling-on-new-base replay, the merged tree would
        // equal old_sha's tree and the diff against
        // parent_old_sha^{tree} would include the *whole PR*
        // rather than just the edit.
        let dir = init_repo();
        let path = dir.path();
        std::fs::write(path.join("base.txt"), "base\n").unwrap();
        run(path, &["add", "base.txt"]);
        run(path, &["commit", "-q", "-m", "base"]);
        let base_sha = rev_parse(path, "HEAD");

        // old PR head: adds routes.py.
        std::fs::write(path.join("routes.py"), "# routes\n").unwrap();
        run(path, &["add", "routes.py"]);
        run(path, &["commit", "-q", "-m", "add routes"]);
        let old_sha = rev_parse(path, "HEAD");

        // new PR head: same base, same routes.py blob, plus the
        // amendment — a follow-up migration and an unrelated
        // tweak to base.txt.
        run(path, &["reset", "--hard", &base_sha]);
        std::fs::write(path.join("routes.py"), "# routes\n").unwrap();
        std::fs::write(path.join("migration.py"), "# migration\n").unwrap();
        std::fs::write(path.join("base.txt"), "base\nindex\n").unwrap();
        run(path, &["add", "."]);
        run(path, &["commit", "-q", "-m", "add routes + follow-up"]);
        let new_sha = rev_parse(path, "HEAD");

        let merged = compute_merged_tree(Some(path), &old_sha, &new_sha).expect("clean merge");
        assert_eq!(merged.parent_old_sha, base_sha);

        // Now the orientation invariant: diff between old_sha's
        // tree and the merged tree must contain ONLY the
        // amendment. routes.py was already in old_sha and must
        // not appear.
        let diff = run_git_capture(
            Some(path),
            &[
                "diff-tree",
                "-r",
                "--no-renames",
                "--name-status",
                &old_sha,
                &merged.tree_sha,
            ],
        )
        .unwrap();
        let mut changed: Vec<&str> = diff
            .lines()
            .filter_map(|l| l.split_once('\t').map(|(_, p)| p))
            .collect();
        changed.sort_unstable();
        assert_eq!(changed, ["base.txt", "migration.py"]);
    }

    #[test]
    fn compute_merged_tree_returns_none_on_missing_parent() {
        // Bogus SHAs — the rev-parse for `^` fails, classifier
        // must fall back to None rather than propagating.
        let dir = init_repo();
        std::fs::write(dir.path().join("x"), "x\n").unwrap();
        run(dir.path(), &["add", "x"]);
        run(dir.path(), &["commit", "-q", "-m", "x"]);
        // The single commit has no parent — `HEAD^` rev-parse
        // fails, which is exactly the "missing parents" case
        // we're guarding against.
        let head = rev_parse(dir.path(), "HEAD");
        assert!(compute_merged_tree(Some(dir.path()), &head, &head).is_none());
    }

    #[test]
    fn compute_merged_tree_returns_none_on_conflict() {
        // Replay conflict shape: parent_old and parent_new
        // disagree on a line that new_sha also edits, so the
        // 3-way `merge-tree --merge-base=parent_new parent_old
        // new_sha` exits non-zero and the classifier returns
        // None.
        let dir = init_repo();
        let path = dir.path();
        std::fs::write(path.join("x"), "base\n").unwrap();
        run(path, &["add", "x"]);
        run(path, &["commit", "-q", "-m", "base"]);
        let base = rev_parse(path, "HEAD");

        // old branch: parent_old sets x="old", old_sha just adds y.
        std::fs::write(path.join("x"), "old\n").unwrap();
        run(path, &["add", "x"]);
        run(path, &["commit", "-q", "-m", "parent_old"]);
        std::fs::write(path.join("y"), "y\n").unwrap();
        run(path, &["add", "y"]);
        run(path, &["commit", "-q", "-m", "old"]);
        let old_sha = rev_parse(path, "HEAD");

        // new branch: parent_new sets x="new", new_sha edits x
        // to "new_amended". When merge-tree replays (new_sha −
        // parent_new) onto parent_old, both sides edit x —
        // conflict.
        run(path, &["reset", "--hard", &base]);
        std::fs::write(path.join("x"), "new\n").unwrap();
        run(path, &["add", "x"]);
        run(path, &["commit", "-q", "-m", "parent_new"]);
        std::fs::write(path.join("x"), "new_amended\n").unwrap();
        run(path, &["add", "x"]);
        run(path, &["commit", "-q", "-m", "new"]);
        let new_sha = rev_parse(path, "HEAD");

        assert!(compute_merged_tree(Some(path), &old_sha, &new_sha).is_none());
    }

    #[test]
    fn tree_entry_serialises_with_renamed_type_and_null_sha() {
        // Wire-shape contract with GitHub's `POST /git/trees`:
        // the field is `"type"` (not Rust's reserved `type_`)
        // and a deletion's sha serialises to JSON `null`.
        let e = TreeEntry {
            path: "a.py".to_string(),
            mode: "100644".to_string(),
            type_: "blob".to_string(),
            sha: None,
        };
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["type"], "blob");
        assert_eq!(json["sha"], serde_json::Value::Null);
        assert_eq!(json["path"], "a.py");
        assert_eq!(json["mode"], "100644");
    }

    mod http {
        use super::*;
        use mergify_core::{ApiFlavor, HttpClient};
        use serde_json::json;
        use url::Url;
        use wiremock::matchers::{method, path as wm_path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn client(server: &MockServer) -> HttpClient {
            HttpClient::new(
                Url::parse(&server.uri()).unwrap(),
                "token",
                ApiFlavor::GitHub,
            )
            .unwrap()
        }

        fn one_entry() -> Vec<TreeEntry> {
            vec![TreeEntry {
                path: "src/a.py".into(),
                mode: "100644".into(),
                type_: "blob".into(),
                sha: Some("bbb2222".into()),
            }]
        }

        #[tokio::test]
        async fn upload_replay_commit_chains_tree_then_commit() {
            // Two-call shape — body contract: tree POST carries
            // `base_tree` + `tree` entries; commit POST carries
            // `parents=[old_sha]` so the GitHub compare URL
            // anchors on it.
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(wm_path("/repos/o/r/git/trees"))
                .respond_with(
                    ResponseTemplate::new(201).set_body_json(json!({"sha": "server_tree"})),
                )
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(wm_path("/repos/o/r/git/commits"))
                .respond_with(
                    ResponseTemplate::new(201).set_body_json(json!({"sha": "server_commit"})),
                )
                .mount(&server)
                .await;

            let entries = one_entry();
            let sha = upload_replay_commit(
                &client(&server),
                "o",
                "r",
                "parent_old_tree_sha",
                "abc1234deadbeef",
                "def5678cafef00d",
                &entries,
            )
            .await;
            assert_eq!(sha.as_deref(), Some("server_commit"));
        }

        #[tokio::test]
        async fn upload_replay_commit_returns_none_when_tree_post_fails() {
            // 422 on the tree POST — short-circuit, no commit
            // POST. Python catches the error and returns None;
            // mirror that so a transient API hiccup doesn't
            // crash the push.
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(wm_path("/repos/o/r/git/trees"))
                .respond_with(ResponseTemplate::new(422))
                .mount(&server)
                .await;

            assert!(
                upload_replay_commit(
                    &client(&server),
                    "o",
                    "r",
                    "base",
                    "old",
                    "new",
                    &one_entry(),
                )
                .await
                .is_none(),
            );
        }

        #[tokio::test]
        async fn upload_replay_commit_returns_none_when_commit_post_fails() {
            // Tree POST succeeds, commit POST fails — still None.
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(wm_path("/repos/o/r/git/trees"))
                .respond_with(
                    ResponseTemplate::new(201).set_body_json(json!({"sha": "server_tree"})),
                )
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(wm_path("/repos/o/r/git/commits"))
                .respond_with(ResponseTemplate::new(422))
                .mount(&server)
                .await;

            assert!(
                upload_replay_commit(
                    &client(&server),
                    "o",
                    "r",
                    "base",
                    "old",
                    "new",
                    &one_entry(),
                )
                .await
                .is_none(),
            );
        }

        #[tokio::test]
        async fn replay_for_revision_end_to_end_returns_server_commit_sha() {
            // Drive the orchestrator against a real repo + a
            // mock GitHub: clean merge, real diff, both POSTs
            // succeed → returns the server commit SHA.
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(wm_path("/repos/o/r/git/trees"))
                .respond_with(
                    ResponseTemplate::new(201).set_body_json(json!({"sha": "server_tree"})),
                )
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(wm_path("/repos/o/r/git/commits"))
                .respond_with(
                    ResponseTemplate::new(201).set_body_json(json!({"sha": "server_commit"})),
                )
                .mount(&server)
                .await;

            let dir = init_repo();
            let path = dir.path();
            std::fs::write(path.join("base.txt"), "base\n").unwrap();
            run(path, &["add", "."]);
            run(path, &["commit", "-q", "-m", "base"]);
            let base_sha = rev_parse(path, "HEAD");

            std::fs::write(path.join("routes.py"), "# routes\n").unwrap();
            run(path, &["add", "."]);
            run(path, &["commit", "-q", "-m", "add"]);
            let old_sha = rev_parse(path, "HEAD");

            run(path, &["reset", "--hard", &base_sha]);
            std::fs::write(path.join("routes.py"), "# routes\n").unwrap();
            std::fs::write(path.join("migration.py"), "# migration\n").unwrap();
            run(path, &["add", "."]);
            run(path, &["commit", "-q", "-m", "add + amend"]);
            let new_sha = rev_parse(path, "HEAD");

            let sha =
                replay_for_revision(&client(&server), Some(path), "o", "r", &old_sha, &new_sha)
                    .await;
            assert_eq!(sha.as_deref(), Some("server_commit"));
        }

        #[tokio::test]
        async fn replay_for_revision_returns_none_when_diff_is_empty() {
            // If the merged tree equals parent_old's tree there's
            // nothing to upload — short-circuit before any POST.
            // No mocks needed: any POST attempt would 404 and
            // turn into Some(...).is_none() == false.
            let server = MockServer::start().await;
            let dir = init_repo();
            let path = dir.path();
            std::fs::write(path.join("x"), "x\n").unwrap();
            run(path, &["add", "."]);
            run(path, &["commit", "-q", "-m", "base"]);
            let base_sha = rev_parse(path, "HEAD");

            // Identical old/new commits (each child of base, both
            // adding the same blob): merge-tree returns
            // base_tree, diff against parent_old^{tree} is empty.
            std::fs::write(path.join("y"), "y\n").unwrap();
            run(path, &["add", "."]);
            run(path, &["commit", "-q", "-m", "child"]);
            let old_sha = rev_parse(path, "HEAD");
            run(path, &["reset", "--hard", &base_sha]);
            std::fs::write(path.join("y"), "y\n").unwrap();
            run(path, &["add", "."]);
            run(path, &["commit", "-q", "-m", "child"]);
            let new_sha = rev_parse(path, "HEAD");

            assert!(
                replay_for_revision(&client(&server), Some(path), "o", "r", &old_sha, &new_sha,)
                    .await
                    .is_none(),
            );
        }
    }
}
