//! Git-level operations the `stack push` orchestrator runs before
//! and during the actual `git push`:
//!
//! - [`fetch_notes_ref`] — learn the remote's current state of
//!   `refs/notes/mergify/stack` before the push, returning a
//!   [`NotesLease`] that anchors the notes push's
//!   `--force-with-lease` (so a concurrent notes push landing
//!   between our fetch and our push is rejected instead of
//!   silently clobbered). Unifies the "first push" (ref missing
//!   both locally and remotely) and "follow-up push" (local
//!   notes from `stack note`, remote notes from prior pushes —
//!   merge them union-style) paths into one call.
//! - [`push_branches`] — the actual `git push --atomic
//!   --force-with-lease …` that lands every create/update from
//!   the planned changes plus the notes ref in one shot. Because
//!   the notes ref rides along in the same `--atomic` push as the
//!   branches, a stale notes lease fails the *whole* push —
//!   branches included. That's intentional, not a rough edge: the
//!   caller's retry re-fetches (getting a fresh [`NotesLease`])
//!   and union-merges before trying again, so nothing is lost.
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

/// What we learned about the remote `refs/notes/mergify/stack`
/// while fetching — the lease anchor for the notes push.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotesLease {
    /// Remote ref seen at this SHA: lease the push on it, so a
    /// concurrent notes push between our fetch and our push is
    /// rejected instead of clobbered.
    RemoteAt(String),
    /// Remote ref doesn't exist: lease on "ref must not exist".
    RemoteAbsent,
    /// Transient failure talking to the remote — its state is
    /// unknown, so no safe lease exists. The notes refspec is
    /// left out of the push entirely; branches still land and
    /// the notes catch up on the next push.
    Unknown,
}

/// Learn the remote's current state of `refs/notes/mergify/stack`
/// so the next push can lease its own notes push on it — see
/// [`NotesLease`].
///
/// When the ref already exists locally (e.g. from a prior
/// `stack note`), we fetch the remote's copy into a temp ref,
/// capture its SHA as the lease anchor *before* touching anything
/// else, then union-merge it into the local ref so unpushed local
/// notes survive. The merge itself is best-effort — its failure is
/// swallowed so a transient remote hiccup doesn't block the
/// surrounding push — but a lease was already captured either way.
///
/// When the ref is missing locally, we fetch it directly: success
/// captures the newly-landed ref's SHA as `RemoteAt`; `couldn't
/// find remote ref` means "first push of the stack", which maps to
/// `RemoteAbsent`; any other failure propagates as `Err` so the
/// user sees what went wrong (network, auth, …) — unchanged from
/// before this function returned a lease type.
pub fn fetch_notes_ref(repo_dir: Option<&Path>, remote: &str) -> Result<NotesLease, CliError> {
    // Ref exists locally → fetch-and-merge the remote in, capturing
    // whatever lease that path can construct.
    if run_git_silent(repo_dir, &["rev-parse", "--verify", STACK_NOTES_REF]).is_ok() {
        return Ok(fetch_and_merge_remote_notes(repo_dir, remote));
    }

    // Ref missing locally → try to fetch it. `couldn't find
    // remote ref` means "first push of the stack", which is a
    // benign `RemoteAbsent`. Any other failure propagates so the
    // user sees what went wrong (network, auth, …).
    let refspec = format!("{STACK_NOTES_REF}:{STACK_NOTES_REF}");
    let output = git_cmd(repo_dir)
        .args(["fetch", remote, "--no-write-fetch-head", &refspec])
        .output()
        .map_err(|e| CliError::Generic(format!("failed to spawn `git fetch`: {e}")))?;
    if output.status.success() {
        // Capture immediately — nothing else writes to the ref on
        // this path, but the lease must reflect exactly what the
        // fetch just landed.
        let sha = run_git_capture(repo_dir, &["rev-parse", "--verify", STACK_NOTES_REF])?;
        return Ok(NotesLease::RemoteAt(sha));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("couldn't find remote ref") {
        return Ok(NotesLease::RemoteAbsent);
    }
    Err(CliError::Generic(format!(
        "`git fetch {remote} {refspec}` failed: {}",
        stderr.trim()
    )))
}

/// The "local notes ref already exists" path of [`fetch_notes_ref`]:
/// fetch the remote's copy into [`NOTES_INCOMING_REF`], capture its
/// SHA as the lease anchor, then union-merge it into the local ref.
///
/// Never errors — a transient fetch failure (network, auth, remote
/// gone) yields `NotesLease::Unknown` rather than blocking the
/// surrounding push, which only needs *a* branch push to succeed.
fn fetch_and_merge_remote_notes(repo_dir: Option<&Path>, remote: &str) -> NotesLease {
    let refspec = format!("+{STACK_NOTES_REF}:{NOTES_INCOMING_REF}");
    let Ok(output) = git_cmd(repo_dir)
        .args(["fetch", remote, "--no-write-fetch-head", &refspec])
        .output()
    else {
        return NotesLease::Unknown;
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return if stderr.contains("couldn't find remote ref") {
            NotesLease::RemoteAbsent
        } else {
            NotesLease::Unknown
        };
    }

    // Capture the remote SHA *before* the union-merge below writes
    // to the local ref — the lease must anchor on exactly what the
    // remote had at fetch time.
    let lease = run_git_capture(repo_dir, &["rev-parse", "--verify", NOTES_INCOMING_REF])
        .map_or(NotesLease::Unknown, NotesLease::RemoteAt);

    // `notes merge --strategy=union` keeps both sides verbatim
    // when they diverge. Stack notes are either plain amend
    // reasons or full history notes keyed by SHA; a union may
    // concatenate two divergent history notes, which
    // `revision_note::parse` tolerates (it keeps the longest
    // marker payload) and the next push rewrites cleanly.
    let notes_ref_arg = format!("--ref={STACK_NOTES_REF}");
    let _ = run_git_silent(
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

    lease
}

/// `git push --atomic [--no-verify] --force-with-lease=… …` —
/// land every Create/Update entry plus the notes ref in one
/// atomic push.
///
/// `notes_lease` is the [`NotesLease`] [`fetch_notes_ref`] returned
/// during pre-flight:
/// - `RemoteAt(sha)` leases the notes refspec on that SHA.
/// - `RemoteAbsent` leases on the empty string ("ref must not
///   exist"), protecting the create race too.
/// - `Unknown` (the remote's state couldn't be determined) drops
///   the notes lease *and* the notes refspec from this push
///   entirely, rather than force-pushing blind — branches still
///   land, and the notes catch up on the next push.
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
    notes_lease: &NotesLease,
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
    // exists — there's nothing to push otherwise. When the lease
    // is `Unknown` (remote state couldn't be determined at fetch
    // time), skip the notes refspec entirely rather than force-
    // push blind: branches still land, and the notes catch up on
    // the next push.
    let notes_local_sha =
        run_git_capture(repo_dir, &["rev-parse", "--verify", STACK_NOTES_REF]).ok();
    let mut include_notes_refspec = false;
    if notes_local_sha.is_some() {
        match notes_lease {
            NotesLease::RemoteAt(sha) => {
                args.push(format!("--force-with-lease={STACK_NOTES_REF}:{sha}"));
                include_notes_refspec = true;
            }
            NotesLease::RemoteAbsent => {
                args.push(format!("--force-with-lease={STACK_NOTES_REF}:"));
                include_notes_refspec = true;
            }
            NotesLease::Unknown => {}
        }
    }

    args.push(remote.to_string());
    for entry in entries {
        args.push(format!(
            "{}:refs/heads/{}",
            entry.commit_sha, entry.dest_branch,
        ));
    }
    if include_notes_refspec {
        // No leading `+`: the refspec must go through the
        // `--force-with-lease` above rather than bypass it. A `+`
        // here would make git ignore the lease and force-push
        // unconditionally, which is exactly the bug this fixes.
        args.push(format!("{STACK_NOTES_REF}:{STACK_NOTES_REF}"));
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
    fn fetch_notes_ref_returns_remote_absent_on_first_push_when_neither_side_has_ref() {
        // Bare remote with no notes ref + local repo with no
        // notes ref → first-push path. Must return `RemoteAbsent`
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
        assert_eq!(got, NotesLease::RemoteAbsent);
        // Local ref must still not exist (we didn't fabricate one).
        assert!(rev_parse(local.path(), STACK_NOTES_REF).is_none());
    }

    #[test]
    fn fetch_notes_ref_returns_remote_absent_when_local_ref_already_exists() {
        // Local `stack note` already created the ref, but the
        // remote has nothing — the fetch-and-merge path's fetch
        // fails with "couldn't find remote ref", which maps to
        // `RemoteAbsent` rather than erroring.
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
        assert_eq!(got, NotesLease::RemoteAbsent);
    }

    #[test]
    fn fetch_notes_ref_returns_remote_at_when_only_remote_has_ref() {
        // Remote has notes (someone else pushed first); local
        // is empty. The fetch lands the ref locally and the
        // lease anchors on the remote's SHA.
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
        let remote_sha = rev_parse(bare.path(), STACK_NOTES_REF).unwrap();

        // Fresh consumer with no notes ref locally.
        let local = init_repo();
        seed_commit(local.path(), "y", "y\n", "y");
        run(
            local.path(),
            &["remote", "add", "origin", bare.path().to_str().unwrap()],
        );
        assert!(rev_parse(local.path(), STACK_NOTES_REF).is_none());

        let got = fetch_notes_ref(Some(local.path()), "origin").unwrap();
        assert_eq!(got, NotesLease::RemoteAt(remote_sha));
        // The fetch lands the notes ref locally.
        assert!(rev_parse(local.path(), STACK_NOTES_REF).is_some());
    }

    #[test]
    fn fetch_notes_ref_returns_unknown_when_remote_unreachable() {
        // Local ref exists (so the fetch-and-merge path runs), but
        // the remote URL doesn't resolve to anything — the fetch
        // fails for a reason other than "couldn't find remote
        // ref", so no safe lease can be constructed.
        let local = init_repo();
        let sha = seed_commit(local.path(), "x", "x\n", "x");
        let notes_ref_arg = format!("--ref={STACK_NOTES_REF}");
        run(
            local.path(),
            &["notes", &notes_ref_arg, "add", "-m", "why", &sha],
        );
        let bogus_remote = local.path().join("no-such-remote");
        run(
            local.path(),
            &["remote", "add", "origin", bogus_remote.to_str().unwrap()],
        );

        let got = fetch_notes_ref(Some(local.path()), "origin").unwrap();
        assert_eq!(got, NotesLease::Unknown);
    }

    #[test]
    fn push_branches_is_noop_when_entries_empty() {
        // No entries → no git push call. Bare remote isn't even
        // queried (and if it were the result would still be Ok
        // since git push with no refspecs is a no-op).
        let local = init_repo();
        seed_commit(local.path(), "x", "x\n", "x");
        push_branches(
            Some(local.path()),
            "origin",
            &[],
            false,
            &NotesLease::RemoteAbsent,
        )
        .unwrap();
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
        push_branches(
            Some(local.path()),
            "origin",
            &entries,
            false,
            &NotesLease::RemoteAbsent,
        )
        .unwrap();

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
        let err = push_branches(
            Some(local.path()),
            "origin",
            &entries,
            false,
            &NotesLease::RemoteAbsent,
        )
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

    #[test]
    fn push_branches_notes_lease_rejects_when_remote_moved() {
        // The core safety invariant this fix exists for: a lease
        // captured at fetch time (`RemoteAt(v1)`) must reject the
        // push once a concurrent `stack push` has moved the remote
        // notes ref to `v2` — and the remote must be left exactly
        // as producer B left it (no silent clobber, no partial
        // atomic landing of the branch either).
        let bare = init_bare_repo();
        let notes_ref_arg = format!("--ref={STACK_NOTES_REF}");

        // Producer A seeds the remote with a commit + a note.
        let producer_a = init_repo();
        let sha_a = seed_commit(producer_a.path(), "a", "a\n", "a");
        run(
            producer_a.path(),
            &["remote", "add", "origin", bare.path().to_str().unwrap()],
        );
        run(
            producer_a.path(),
            &["notes", &notes_ref_arg, "add", "-m", "a", &sha_a],
        );
        run(producer_a.path(), &["push", "origin", "main"]);
        let force_notes_refspec = format!("+{STACK_NOTES_REF}:{STACK_NOTES_REF}");
        run(producer_a.path(), &["push", "origin", &force_notes_refspec]);

        // Consumer fetches: local ref is missing, so `fetch_notes_ref`
        // takes the fetch-and-capture path, lands the notes ref
        // locally at v1, and returns `RemoteAt(v1)`.
        let consumer = init_repo();
        let consumer_sha = seed_commit(consumer.path(), "y", "y\n", "y");
        run(
            consumer.path(),
            &["remote", "add", "origin", bare.path().to_str().unwrap()],
        );
        let lease = fetch_notes_ref(Some(consumer.path()), "origin").unwrap();
        let NotesLease::RemoteAt(v1) = lease.clone() else {
            panic!("expected RemoteAt, got {lease:?}");
        };
        assert_eq!(
            rev_parse(consumer.path(), STACK_NOTES_REF),
            Some(v1.clone())
        );

        // Producer B force-pushes a divergent notes state to the
        // remote after the consumer's fetch — the remote moves to v2.
        let producer_b = init_repo();
        let sha_b = seed_commit(producer_b.path(), "b", "b\n", "b");
        run(
            producer_b.path(),
            &["remote", "add", "origin", bare.path().to_str().unwrap()],
        );
        run(
            producer_b.path(),
            &["notes", &notes_ref_arg, "add", "-m", "b", &sha_b],
        );
        run(producer_b.path(), &["push", "origin", &force_notes_refspec]);
        let v2 = rev_parse(bare.path(), STACK_NOTES_REF).unwrap();
        assert_ne!(v1, v2, "producer B must have moved the remote notes ref");

        // Consumer adds a local note (still built on the stale v1)
        // and pushes with the now-stale `RemoteAt(v1)` lease.
        run(
            consumer.path(),
            &["notes", &notes_ref_arg, "add", "-m", "y", &consumer_sha],
        );
        let entries = vec![PushEntry {
            commit_sha: consumer_sha,
            dest_branch: "stack/feat/y".into(),
            pull_head_sha: None,
        }];
        push_branches(Some(consumer.path()), "origin", &entries, false, &lease)
            .expect_err("stale notes lease must reject the whole atomic push");

        // Remote notes ref unchanged → no silent clobber. The
        // branch must not have landed either (atomic push).
        assert_eq!(rev_parse(bare.path(), STACK_NOTES_REF).unwrap(), v2);
        assert!(
            rev_parse(bare.path(), "refs/heads/stack/feat/y").is_none(),
            "atomic push must not land the branch when the notes lease fails"
        );
    }

    #[test]
    fn push_branches_notes_lease_succeeds_when_lease_current() {
        // The positive case: when nothing raced, the lease is
        // still current at push time and the push (branch + notes)
        // lands normally, moving the remote notes ref to the
        // consumer's local SHA.
        let bare = init_bare_repo();
        let notes_ref_arg = format!("--ref={STACK_NOTES_REF}");

        // Producer seeds the remote with a commit + a note.
        let producer = init_repo();
        let sha_a = seed_commit(producer.path(), "a", "a\n", "a");
        run(
            producer.path(),
            &["remote", "add", "origin", bare.path().to_str().unwrap()],
        );
        run(
            producer.path(),
            &["notes", &notes_ref_arg, "add", "-m", "a", &sha_a],
        );
        run(producer.path(), &["push", "origin", "main"]);
        let force_notes_refspec = format!("+{STACK_NOTES_REF}:{STACK_NOTES_REF}");
        run(producer.path(), &["push", "origin", &force_notes_refspec]);

        // Consumer fetches, getting `RemoteAt(v1)` and landing the
        // notes ref locally at v1.
        let consumer = init_repo();
        let consumer_sha = seed_commit(consumer.path(), "y", "y\n", "y");
        run(
            consumer.path(),
            &["remote", "add", "origin", bare.path().to_str().unwrap()],
        );
        let lease = fetch_notes_ref(Some(consumer.path()), "origin").unwrap();
        assert!(matches!(lease, NotesLease::RemoteAt(_)));

        // Consumer adds a local note, extending the notes ref past
        // v1, then pushes with the still-current lease.
        run(
            consumer.path(),
            &["notes", &notes_ref_arg, "add", "-m", "y", &consumer_sha],
        );
        let local_notes_sha = rev_parse(consumer.path(), STACK_NOTES_REF).unwrap();
        let entries = vec![PushEntry {
            commit_sha: consumer_sha,
            dest_branch: "stack/feat/y".into(),
            pull_head_sha: None,
        }];
        push_branches(Some(consumer.path()), "origin", &entries, false, &lease).unwrap();

        // Remote notes ref moved to the consumer's local SHA.
        assert_eq!(
            rev_parse(bare.path(), STACK_NOTES_REF).unwrap(),
            local_notes_sha
        );
    }
}
