//! Tiny helpers for shelling out to `git`.
//!
//! The engine publishes merge-queue metadata as git notes under
//! `refs/notes/mergify/*`. Both [`crate::git_refs`] and
//! [`crate::queue_info`] fetch and read those notes the same way: run
//! git, silence stderr, and treat any failure (no remote, no notes,
//! not a repo) as "not found" so the caller falls through to another
//! detection path. These two functions are that shared plumbing.

use std::process::Command;
use std::process::Stdio;

/// Read the engine's merge-queue note at `notes_ref` for `rev`
/// (`git notes --ref=<notes_ref> show <rev>`) and return its full
/// payload as a JSON value. `notes_ref` may be the short
/// `mergify/<branch>` form or a fully-qualified
/// `refs/notes/mergify/<branch>` — git accepts both.
///
/// The note body is the engine's `TrainInfo` YAML dump. We parse it
/// into a generic value rather than a fixed struct so the whole payload
/// is preserved — every field, including any the engine adds later.
/// Both `queue_info` (which prints the whole note) and `git_refs`
/// (which deserializes it into a typed view to pull `checking_base_sha`)
/// read it through here.
///
/// Returns `None` when there's no note at `rev`, or the body isn't a
/// YAML mapping.
#[must_use]
pub(crate) fn read_note(notes_ref: &str, rev: &str) -> Option<serde_json::Value> {
    let content = capture(&["notes", &format!("--ref={notes_ref}"), "show", rev])?;
    parse_note(&content)
}

/// Parse a note body (YAML) into a JSON value, keeping every field.
/// Restricted to mappings: a bare scalar or sequence isn't a
/// merge-queue note and would make the JSON output meaningless.
fn parse_note(content: &str) -> Option<serde_json::Value> {
    let value: serde_json::Value = serde_yaml_ng::from_str(content).ok()?;
    value.is_object().then_some(value)
}

/// Run a git subcommand for its exit status, discarding all output.
/// Returns `true` only on a clean exit.
#[must_use]
pub(crate) fn succeeds(args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Run a git subcommand and capture stdout as UTF-8. Returns `None` on
/// any failure: the process couldn't spawn, exited non-zero, or wrote
/// non-UTF-8.
#[must_use]
pub(crate) fn capture(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_note_keeps_whole_payload() {
        // The note body is the engine's full TrainInfo dump. Every
        // field must survive into the value — including top-level and
        // per-PR `scopes`, which a fixed struct would have dropped.
        let body = "\
scopes:
  - backend
pull_requests:
  - number: 7
    scopes:
      - backend
previous_failed_batches:
  - draft_pr_number: 42
    checked_pull_requests:
      - 7
checking_base_sha: cafef00d
";
        let note = parse_note(body).expect("engine payload should parse");
        assert_eq!(note["checking_base_sha"], "cafef00d");
        assert_eq!(note["scopes"][0], "backend");
        assert_eq!(note["pull_requests"][0]["number"], 7);
        assert_eq!(note["pull_requests"][0]["scopes"][0], "backend");
        assert_eq!(note["previous_failed_batches"][0]["draft_pr_number"], 42);
    }

    #[test]
    fn parse_note_rejects_non_mapping() {
        // A bare scalar or sequence isn't a merge-queue note.
        assert!(parse_note("just a scalar\n").is_none());
        assert!(parse_note("- a\n- b\n").is_none());
    }
}
