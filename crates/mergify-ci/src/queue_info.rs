//! `mergify ci queue-info` — print the current build's merge-queue
//! batch metadata, read from a git note.
//!
//! The engine attaches the batch metadata (a `TrainInfo` YAML dump) to
//! the merge-queue branch head commit as a git note under
//! `refs/notes/mergify/<mq_branch>`, precisely so CI providers can read
//! it with plain git and no GitHub token. This command reads that note
//! for the current `HEAD`, so it works in any CI (GitHub Actions,
//! GitLab, `CircleCI`, Jenkins, ...) wherever the merge-queue branch is
//! checked out. It exits with `INVALID_STATE` when no note is found —
//! i.e. the build is not a merge-queue batch.
//!
//! The note's full payload is emitted verbatim as pretty-printed JSON
//! on stdout — every field the engine wrote, including ones this CLI
//! doesn't model — so new engine attributes show up without a CLI
//! change. When `$GITHUB_OUTPUT` is set (GitHub Actions runner) the
//! command also appends two outputs: the full payload as `queue_metadata`
//! under a random `ghadelimiter_<uuid>` heredoc (the pattern the workflow
//! runtime expects for multi-line outputs), and `last_failed_draft_pr` —
//! the most recent failed batch's draft PR number as a plain single line
//! (empty when there are none), so a workflow can branch on it without
//! parsing the JSON.

use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use mergify_core::CliError;
use mergify_core::Output;
use serde_json::Value;

/// Reads the merge-queue git note attached to the current `HEAD`,
/// returning its full payload.
///
/// The real implementation shells out to `git`; tests inject a stub so
/// the command can be exercised without a checkout.
pub type HeadNoteReader<'a> = &'a dyn Fn() -> Option<Value>;

/// Run the `ci queue-info` command.
pub fn run(output: &mut dyn Output) -> Result<(), CliError> {
    run_with_reader(output, &real_head_note_reader)
}

/// Inner entrypoint with the git-note reader injected so tests can run
/// without a real repository.
fn run_with_reader(
    output: &mut dyn Output,
    head_note_reader: HeadNoteReader<'_>,
) -> Result<(), CliError> {
    let Some(metadata) = head_note_reader() else {
        return Err(CliError::InvalidState(
            "No merge queue metadata found. queue-info reads the \
             refs/notes/mergify/<branch> git note the engine attaches to a \
             merge queue draft branch head; run it on a merge queue batch \
             build."
                .to_string(),
        ));
    };

    emit_json(output, &metadata)?;
    write_github_output(&metadata)?;
    Ok(())
}

/// Production [`HeadNoteReader`]: read the merge-queue note attached to
/// the current `HEAD` commit using only plain git, no GitHub token.
///
/// The engine publishes the batch metadata as a git note under
/// `refs/notes/mergify/<mq_branch>` attached to the MQ branch head
/// commit. In CI the working tree is checked out at that commit, so we
/// fetch every `refs/notes/mergify/*` ref and return the note that is
/// attached to `HEAD`. Enumerating the refs rather than deriving the
/// branch name keeps this working under a detached `HEAD` (how most
/// non-GitHub CIs check out a revision) and needs no CI-specific env.
///
/// Any git failure (no remote, notes never published, not a repo, a
/// shallow clone that hid the commit) is swallowed as `None` so the
/// caller surfaces `INVALID_STATE`.
#[must_use]
pub fn real_head_note_reader() -> Option<Value> {
    // Notes aren't fetched by default, so pull them ourselves. The
    // wildcard refspec mirrors the engine's `refs/notes/mergify/*`
    // namespace; `+` force-updates so a re-queued branch's note wins.
    if !crate::git::succeeds(&[
        "fetch",
        "--no-tags",
        "--quiet",
        "origin",
        "+refs/notes/mergify/*:refs/notes/mergify/*",
    ]) {
        return None;
    }
    let refs =
        crate::git::capture(&["for-each-ref", "--format=%(refname)", "refs/notes/mergify/"])?;
    refs.lines()
        .filter(|r| !r.is_empty())
        .find_map(|notes_ref| crate::git::read_note(notes_ref, "HEAD"))
}

fn emit_json(output: &mut dyn Output, metadata: &Value) -> std::io::Result<()> {
    output.emit(metadata, &mut |w: &mut dyn Write| {
        let rendered = serde_json::to_string_pretty(metadata)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        writeln!(w, "{rendered}")
    })
}

fn write_github_output(metadata: &Value) -> Result<(), CliError> {
    let Some(path) = env::var("GITHUB_OUTPUT").ok().filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    let delimiter = format!("ghadelimiter_{}", random_delimiter_suffix()?);
    let compact = serde_json::to_string(metadata)
        .map_err(|e| CliError::Generic(format!("failed to serialize queue metadata: {e}")))?;
    // Surface the most recent failed batch's draft PR number as a plain
    // single-line output so workflows don't have to parse the
    // `queue_metadata` JSON. `previous_failed_batches` is ordered
    // oldest→newest, so the last element is the most recent. Empty string
    // when the field is absent or empty, which makes the output falsy
    // (`if: steps.x.outputs.last_failed_draft_pr`).
    let last_failed_draft_pr = metadata
        .get("previous_failed_batches")
        .and_then(Value::as_array)
        .and_then(|batches| batches.last())
        .and_then(|batch| batch.get("draft_pr_number"))
        .and_then(Value::as_u64)
        .map(|n| n.to_string())
        .unwrap_or_default();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(PathBuf::from(path))?;
    writeln!(file, "queue_metadata<<{delimiter}")?;
    writeln!(file, "{compact}")?;
    writeln!(file, "{delimiter}")?;
    writeln!(file, "last_failed_draft_pr={last_failed_draft_pr}")?;
    Ok(())
}

/// 16 random bytes rendered as 32 lowercase hex chars — enough
/// entropy to be unguessable inside one GitHub Actions step, which
/// is all the heredoc delimiter needs (it just has to be absent
/// from the metadata payload). `getrandom` reads from the OS RNG
/// directly; we don't need the UUID parsing/formatting plumbing
/// that `uuid` adds on top.
fn random_delimiter_suffix() -> Result<String, CliError> {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf)
        .map_err(|e| CliError::Generic(format!("OS random source unavailable: {e}")))?;
    let mut hex = String::with_capacity(buf.len() * 2);
    for b in buf {
        use std::fmt::Write as _;
        write!(hex, "{b:02x}").expect("writing to String is infallible");
    }
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use mergify_core::ExitCode;
    use mergify_test_support::Captured;
    use serde_json::json;

    use super::*;

    /// A note carrying fields the CLI doesn't model (`scopes`,
    /// per-PR `scopes`) — they must still reach the output.
    fn sample() -> Value {
        json!({
            "checking_base_sha": "abc123",
            "pull_requests": [{"number": 10, "scopes": ["backend"]}],
            "previous_failed_batches": [],
            "scopes": ["backend"],
        })
    }

    fn no_note() -> Option<Value> {
        None
    }

    #[test]
    fn errors_when_no_note() {
        let mut cap = Captured::human();
        let err = run_with_reader(&mut cap.output, &no_note).unwrap_err();
        assert!(matches!(err, CliError::InvalidState(_)));
        assert_eq!(err.exit_code(), ExitCode::InvalidState);
    }

    #[test]
    fn prints_whole_note_payload() {
        let note = || Some(sample());
        let mut cap = Captured::human();
        temp_env::with_var("GITHUB_OUTPUT", None::<&str>, || {
            run_with_reader(&mut cap.output, &note).unwrap();
        });
        let stdout = cap.stdout();
        assert!(stdout.contains("\"checking_base_sha\": \"abc123\""));
        assert!(stdout.contains("\"number\": 10"));
        // Fields the CLI doesn't model are passed through verbatim.
        assert!(stdout.contains("\"scopes\""), "scopes missing: {stdout}");
        assert!(
            stdout.contains("\"backend\""),
            "scope value missing: {stdout}"
        );
    }

    #[test]
    fn appends_to_github_output_when_set() {
        let dir = tempfile::tempdir().unwrap();
        let gha_output = dir.path().join("gha_output");
        let note = || Some(sample());
        let mut cap = Captured::human();
        temp_env::with_var("GITHUB_OUTPUT", Some(gha_output.to_str().unwrap()), || {
            run_with_reader(&mut cap.output, &note).unwrap();
        });
        let written = std::fs::read_to_string(&gha_output).unwrap();
        assert!(written.starts_with("queue_metadata<<ghadelimiter_"));
        assert!(written.contains("\"checking_base_sha\":\"abc123\""));
        assert!(written.contains("\"scopes\":[\"backend\"]"));
        // `sample()` has an empty `previous_failed_batches`, so the
        // convenience output is present but empty (falsy in workflow `if:`).
        assert!(
            written.contains("\nlast_failed_draft_pr=\n"),
            "got: {written:?}"
        );
    }

    #[test]
    fn github_output_exposes_last_failed_draft_pr() {
        let dir = tempfile::tempdir().unwrap();
        let gha_output = dir.path().join("gha_output");
        // Two failed batches — the last one (draft PR 99) is the most
        // recent and is what the convenience output must surface.
        let note = || {
            Some(json!({
                "checking_base_sha": "abc123",
                "previous_failed_batches": [
                    {"draft_pr_number": 42, "checked_pull_requests": [1, 2]},
                    {"draft_pr_number": 99, "checked_pull_requests": [3]},
                ],
            }))
        };
        let mut cap = Captured::human();
        temp_env::with_var("GITHUB_OUTPUT", Some(gha_output.to_str().unwrap()), || {
            run_with_reader(&mut cap.output, &note).unwrap();
        });
        let written = std::fs::read_to_string(&gha_output).unwrap();
        assert!(
            written.contains("\nlast_failed_draft_pr=99\n"),
            "got: {written:?}"
        );
    }
}
