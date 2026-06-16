//! Git-level operations the `stack push` orchestrator runs before
//! and during the actual `git push`:
//!
//! - [`fetch_notes_ref`] — make sure `refs/notes/mergify/stack`
//!   exists locally before the push so the
//!   `--force-with-lease=<notes-ref>:<sha>` arg has a SHA to
//!   anchor on. Unifies the "first push" (ref missing both
//!   locally and remotely) and "follow-up push" (local notes
//!   from `stack note`, remote notes from prior pushes — merge
//!   them union-style) paths into one call.
//! - [`push_branches`] — the actual `git push --atomic
//!   --force-with-lease …` that lands every create/update from
//!   the planned changes plus the notes ref in one shot.
//!
//! Ported from `mergify_cli/stack/push.py` —
//! `_merge_remote_notes`, `fetch_notes_ref`, `push_branches`.

use std::path::Path;

use crate::git::{git_cmd, run_git_capture, run_git_silent};

use mergify_core::CliError;

use crate::local_commits::STACK_NOTES_REF;

/// Marker env var: the pre-push hook reads this to know it's
/// being invoked by `mergify stack push` (vs a user's
/// `git push`) and skip its own logic accordingly. Set across
/// the duration of [`push_branches`] only.
const PUSH_ENV_MARKER: &str = "MERGIFY_STACK_PUSH";

/// Temporary fetch ref used by [`fetch_notes_ref`] to land the
/// remote notes before merging them union-style into the local
/// `refs/notes/mergify/stack`.
const NOTES_INCOMING_REF: &str = "refs/notes/mergify/stack-incoming";

/// One PR's worth of refspec input for [`push_branches`]. Built
/// from the orchestrator's planned-changes list — only the
/// `Create`/`Update` entries make it in.
#[derive(Debug, Clone)]
pub struct PushEntry {
    /// SHA of the local commit being pushed.
    pub commit_sha: String,
    /// Remote branch name `mergify stack` chose for this PR.
    pub dest_branch: String,
    /// For `Update`: the remote PR head SHA that `--force-with-lease`
    /// anchors on (so a concurrent push to the same PR fails fast
    /// instead of clobbering work). `None` for `Create`, which
    /// uses an empty lease meaning "ref must not exist."
    pub pull_head_sha: Option<String>,
}

/// Make `refs/notes/mergify/stack` reachable locally before the
/// next push.
///
/// Returns `true` when the local ref is now present (either it
/// pre-existed, or we successfully fetched it). Returns `false`
/// only on the first push of a stack — the ref doesn't exist
/// anywhere yet. The caller uses the boolean to decide whether
/// the push can attach a `--force-with-lease` to the notes ref
/// (it can't on first push because there's no SHA to lease on).
///
/// When the ref already exists locally (e.g. from a prior
/// `stack note`), we union-merge the remote into it so unpushed
/// local notes survive but the lease SHA stays current. Merge
/// failures are swallowed so a transient remote hiccup doesn't
/// block the surrounding push.
pub fn fetch_notes_ref(repo_dir: Option<&Path>, remote: &str) -> Result<bool, CliError> {
    // Ref exists locally → merge the remote in (best-effort) and
    // we're done.
    if run_git_silent(repo_dir, &["rev-parse", "--verify", STACK_NOTES_REF]).is_ok() {
        let _ = merge_remote_notes(repo_dir, remote);
        return Ok(true);
    }

    // Ref missing locally → try to fetch it. `couldn't find
    // remote ref` means "first push of the stack", which is a
    // benign Ok(false). Any other failure propagates so the
    // user sees what went wrong (network, auth, …).
    let refspec = format!("{STACK_NOTES_REF}:{STACK_NOTES_REF}");
    let output = git_cmd(repo_dir)
        .args(["fetch", remote, "--no-write-fetch-head", &refspec])
        .output()
        .map_err(|e| CliError::Generic(format!("failed to spawn `git fetch`: {e}")))?;
    if output.status.success() {
        return Ok(true);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("couldn't find remote ref") {
        return Ok(false);
    }
    Err(CliError::Generic(format!(
        "`git fetch {remote} {refspec}` failed: {}",
        stderr.trim()
    )))
}

fn merge_remote_notes(repo_dir: Option<&Path>, remote: &str) -> Result<(), CliError> {
    let refspec = format!("+{STACK_NOTES_REF}:{NOTES_INCOMING_REF}");
    run_git_silent(
        repo_dir,
        &["fetch", remote, "--no-write-fetch-head", &refspec],
    )?;
    // `notes merge --strategy=union` keeps both sides verbatim
    // when they diverge — for the stack-notes ref the values are
    // amend reasons keyed by SHA, so a union is exactly the
    // right merge.
    let notes_ref_arg = format!("--ref={STACK_NOTES_REF}");
    let result = run_git_silent(
        repo_dir,
        &[
            "notes",
            &notes_ref_arg,
            "merge",
            "--strategy=union",
            NOTES_INCOMING_REF,
        ],
    );
    // Cleanup the tmp ref regardless of merge outcome — Python
    // wraps this in try/finally so we can't leave a dangling
    // `refs/notes/mergify/stack-incoming` behind.
    let _ = run_git_silent(repo_dir, &["update-ref", "-d", NOTES_INCOMING_REF]);
    result
}

/// `git push --atomic [--no-verify] --force-with-lease=… …` —
/// land every Create/Update entry plus the notes ref in one
/// atomic push.
///
/// `notes_ref_fetched` mirrors the boolean from [`fetch_notes_ref`]:
/// when `true` we can attach a `--force-with-lease=<notes-ref>:<sha>`
/// anchor; when `false` (first push) we can't, but we still
/// include the notes-ref refspec so the initial state lands.
///
/// `MERGIFY_STACK_PUSH=1` is exported across the push call so
/// the pre-push hook can tell user pushes apart from CLI pushes
/// and skip its own logic. Cleared on the way out so the env
/// doesn't leak to whatever the orchestrator does next.
pub fn push_branches(
    repo_dir: Option<&Path>,
    remote: &str,
    entries: &[PushEntry],
    no_verify: bool,
    notes_ref_fetched: bool,
) -> Result<(), CliError> {
    if entries.is_empty() {
        return Ok(());
    }

    let mut args: Vec<String> = vec!["push".into(), "--atomic".into()];
    if no_verify {
        args.push("--no-verify".into());
    }

    // One `--force-with-lease` per entry. For Update we lease on
    // the previous PR head SHA so a concurrent push to the same
    // PR fails fast instead of clobbering work. For Create we
    // lease on the empty string, which means "the ref must not
    // exist" — same protection against a Create-vs-Create race.
    for entry in entries {
        let lease = match &entry.pull_head_sha {
            Some(sha) => format!("--force-with-lease=refs/heads/{}:{sha}", entry.dest_branch),
            None => format!("--force-with-lease=refs/heads/{}:", entry.dest_branch),
        };
        args.push(lease);
    }

    // Notes ref lease + refspec only when the local notes ref
    // exists — there's nothing to push otherwise. The lease is
    // skipped on first push (notes_ref_fetched == false) because
    // there's no remote SHA to anchor on; the refspec stays so
    // the initial notes state lands.
    let notes_local_sha =
        run_git_capture(repo_dir, &["rev-parse", "--verify", STACK_NOTES_REF]).ok();
    if let Some(sha) = notes_local_sha.as_deref()
        && notes_ref_fetched
    {
        args.push(format!("--force-with-lease={STACK_NOTES_REF}:{sha}"));
    }

    args.push(remote.to_string());
    for entry in entries {
        args.push(format!(
            "{}:refs/heads/{}",
            entry.commit_sha, entry.dest_branch,
        ));
    }
    if notes_local_sha.is_some() {
        // Leading `+` forces the push even when the remote
        // diverged — paired with `--force-with-lease` above for
        // the actual safety.
        args.push(format!("+{STACK_NOTES_REF}:{STACK_NOTES_REF}"));
    }

    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

    // Scope the env-var to the push so it doesn't leak to
    // whatever the orchestrator does next (logging, comment
    // updates, …). Always cleanup, even on failure.
    let mut cmd = git_cmd(repo_dir);
    cmd.args(&arg_refs).env(PUSH_ENV_MARKER, "1");
    // Capture stdio rather than inherit it: `git push` narrates the
    // branch creation, `remote:` ruleset-bypass notices, and the
    // "create a pull request by visiting …" hint, all of which are
    // noise next to the orchestrator's own plan/created summary.
    // Keep it for the failure path only. Mirrors `run_git_silent`
    // and the Python `utils.git` wrapper.
    let output = cmd
        .output()
        .map_err(|e| CliError::Generic(format!("failed to spawn `git push`: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        return Err(CliError::Generic(if stderr.is_empty() {
            format!("`git push` exited {}", output.status)
        } else {
            format!("`git push` exited {}:\n{stderr}", output.status)
        }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

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

    fn init_bare_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        let path = dir.path();
        run(path, &["init", "-q", "--bare", "-b", "main"]);
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

    fn rev_parse(path: &Path, refname: &str) -> Option<String> {
        run_git_capture(Some(path), &["rev-parse", "--verify", refname]).ok()
    }

    fn seed_commit(path: &Path, name: &str, body: &str, msg: &str) -> String {
        std::fs::write(path.join(name), body).unwrap();
        run(path, &["add", name]);
        run(path, &["commit", "-q", "-m", msg]);
        rev_parse(path, "HEAD").unwrap()
    }

    #[test]
    fn fetch_notes_ref_returns_false_on_first_push_when_neither_side_has_ref() {
        // Bare remote with no notes ref + local repo with no
        // notes ref → first-push path. Must return Ok(false)
        // without erroring so the orchestrator drops the
        // `--force-with-lease` on the notes ref.
        let bare = init_bare_repo();
        let local = init_repo();
        seed_commit(local.path(), "x", "x\n", "x");
        run(
            local.path(),
            &["remote", "add", "origin", bare.path().to_str().unwrap()],
        );

        let got = fetch_notes_ref(Some(local.path()), "origin").unwrap();
        assert!(!got, "first-push path must return Ok(false)");
        // Local ref must still not exist (we didn't fabricate one).
        assert!(rev_parse(local.path(), STACK_NOTES_REF).is_none());
    }

    #[test]
    fn fetch_notes_ref_returns_true_when_local_ref_already_exists() {
        // Local `stack note` already created the ref — function
        // returns Ok(true) without needing the remote to have
        // anything. Best-effort remote-merge runs but its
        // failure (no remote ref) is swallowed.
        let bare = init_bare_repo();
        let local = init_repo();
        let sha = seed_commit(local.path(), "x", "x\n", "x");
        run(
            local.path(),
            &["remote", "add", "origin", bare.path().to_str().unwrap()],
        );
        // Attach a note locally.
        let notes_ref_arg = format!("--ref={STACK_NOTES_REF}");
        run(
            local.path(),
            &["notes", &notes_ref_arg, "add", "-m", "why", &sha],
        );
        assert!(rev_parse(local.path(), STACK_NOTES_REF).is_some());

        let got = fetch_notes_ref(Some(local.path()), "origin").unwrap();
        assert!(got);
    }

    #[test]
    fn fetch_notes_ref_returns_true_when_only_remote_has_ref() {
        // Remote has notes (someone else pushed first); local
        // is empty. The fetch lands the ref locally and we
        // return Ok(true).
        let bare = init_bare_repo();

        // Seed the bare with a commit + a note, then push them.
        let producer = init_repo();
        let sha = seed_commit(producer.path(), "x", "x\n", "x");
        run(
            producer.path(),
            &["remote", "add", "origin", bare.path().to_str().unwrap()],
        );
        let notes_ref_arg = format!("--ref={STACK_NOTES_REF}");
        run(
            producer.path(),
            &["notes", &notes_ref_arg, "add", "-m", "why", &sha],
        );
        let notes_refspec = format!("+{STACK_NOTES_REF}:{STACK_NOTES_REF}");
        run(producer.path(), &["push", "origin", "main", &notes_refspec]);

        // Fresh consumer with no notes ref locally.
        let local = init_repo();
        seed_commit(local.path(), "y", "y\n", "y");
        run(
            local.path(),
            &["remote", "add", "origin", bare.path().to_str().unwrap()],
        );
        assert!(rev_parse(local.path(), STACK_NOTES_REF).is_none());

        let got = fetch_notes_ref(Some(local.path()), "origin").unwrap();
        assert!(got);
        // The fetch lands the notes ref locally.
        assert!(rev_parse(local.path(), STACK_NOTES_REF).is_some());
    }

    #[test]
    fn push_branches_is_noop_when_entries_empty() {
        // No entries → no git push call. Bare remote isn't even
        // queried (and if it were the result would still be Ok
        // since git push with no refspecs is a no-op).
        let local = init_repo();
        seed_commit(local.path(), "x", "x\n", "x");
        push_branches(Some(local.path()), "origin", &[], false, false).unwrap();
    }

    #[test]
    fn push_branches_lands_create_entries_against_a_bare_remote() {
        // End-to-end: a Create entry pushes a new branch to the
        // remote. `pull_head_sha == None` means the lease arg
        // says "ref must not exist", so the push succeeds when
        // the remote ref is absent.
        let bare = init_bare_repo();
        let local = init_repo();
        let sha = seed_commit(local.path(), "x", "x\n", "x");
        run(
            local.path(),
            &["remote", "add", "origin", bare.path().to_str().unwrap()],
        );

        let entries = vec![PushEntry {
            commit_sha: sha.clone(),
            dest_branch: "stack/feat/x".into(),
            pull_head_sha: None,
        }];
        push_branches(Some(local.path()), "origin", &entries, false, false).unwrap();

        // Remote now has the branch pointed at the same SHA.
        let remote_sha =
            run_git_capture(Some(bare.path()), &["rev-parse", "refs/heads/stack/feat/x"]).unwrap();
        assert_eq!(remote_sha, sha);
    }

    #[test]
    fn push_branches_update_entry_fails_when_remote_diverged_from_lease() {
        // Update entry with a stale lease SHA → the
        // `--force-with-lease` rejects the push. Locks in the
        // safety invariant: a concurrent push to the same PR
        // must not silently clobber.
        let bare = init_bare_repo();

        // Producer: seed remote so the branch exists.
        let producer = init_repo();
        let producer_sha = seed_commit(producer.path(), "x", "x\n", "x");
        run(
            producer.path(),
            &["remote", "add", "origin", bare.path().to_str().unwrap()],
        );
        run(
            producer.path(),
            &[
                "push",
                "origin",
                &format!("{producer_sha}:refs/heads/stack/feat/x"),
            ],
        );

        // Local: pretend a different SHA used to be there.
        let local = init_repo();
        let local_sha = seed_commit(local.path(), "y", "y\n", "y");
        run(
            local.path(),
            &["remote", "add", "origin", bare.path().to_str().unwrap()],
        );

        let entries = vec![PushEntry {
            commit_sha: local_sha,
            dest_branch: "stack/feat/x".into(),
            // Stale: not equal to producer_sha on the remote.
            pull_head_sha: Some("0000000000000000000000000000000000000000".into()),
        }];
        let err = push_branches(Some(local.path()), "origin", &entries, false, false)
            .expect_err("stale lease must reject");
        let CliError::Generic(msg) = err else {
            panic!("expected Generic CliError");
        };
        assert!(
            msg.contains("exited"),
            "error must surface a non-zero git push exit ({msg})",
        );

        // Remote ref unchanged → safety invariant holds.
        let remote_sha =
            run_git_capture(Some(bare.path()), &["rev-parse", "refs/heads/stack/feat/x"]).unwrap();
        assert_eq!(remote_sha, producer_sha);
    }
}
