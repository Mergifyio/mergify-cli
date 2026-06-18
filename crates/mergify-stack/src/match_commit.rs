//! Shared formatting for the prefix-matching errors every stack
//! subcommand emits.
//!
//! `drop`, `squash`, `fixup`, `edit`, `move`, `reword`, `reorder`,
//! and `note` each resolve a user-supplied `<COMMIT>` argument
//! against the stack by matching a SHA or Change-Id prefix. The
//! zero-match and ambiguous-match wording must be identical across
//! all of them and match the Python `reorder.py::match_commit`
//! output, so the formatting lives here once.
//!
//! Each command builds a slice of [`Candidate`] (a borrowed view
//! over its own per-commit type, all of which expose
//! `commit_sha` / `title` / `change_id`) and delegates.

use mergify_core::CliError;

/// A single ambiguous-match candidate, borrowed from the caller's
/// per-command commit type. `change_id` is empty when the commit
/// carries no `Change-Id` trailer; the formatter omits the
/// `(<change_id>)` suffix in that case.
pub struct Candidate<'a> {
    pub commit_sha: &'a str,
    pub title: &'a str,
    pub change_id: &'a str,
}

/// `field` is the human label for what the prefix matched against —
/// either `"SHA"` or `"Change-Id"`.
///
/// Zero matches → [`CliError::StackNotFound`] (exit 3), matching
/// Python's `sys.exit(ExitCode.STACK_NOT_FOUND)`.
#[must_use]
pub fn not_found(field: &str, prefix: &str) -> CliError {
    CliError::StackNotFound(format!(
        "no commit found matching {field} prefix '{prefix}'"
    ))
}

/// Ambiguous match → [`CliError::InvalidState`] (exit 7), matching
/// Python's `sys.exit(ExitCode.INVALID_STATE)`.
///
/// The message mirrors `reorder.py::match_commit`:
/// `ambiguous {field} prefix '{prefix}' matches {N} commits:` then
/// one line per candidate `  {sha[:12]} {title} ({change_id[:12]})`
/// (the `(...)` suffix omitted when the candidate has no Change-Id).
#[must_use]
pub fn ambiguous(field: &str, prefix: &str, candidates: &[Candidate<'_>]) -> CliError {
    let listing = candidates
        .iter()
        .map(format_candidate)
        .collect::<Vec<_>>()
        .join("\n  ");
    CliError::InvalidState(format!(
        "ambiguous {field} prefix '{prefix}' matches {n} commits:\n  {listing}",
        n = candidates.len(),
    ))
}

fn format_candidate(c: &Candidate<'_>) -> String {
    let sha = &c.commit_sha[..c.commit_sha.len().min(12)];
    if c.change_id.is_empty() {
        format!("{sha} {title}", title = c.title)
    } else {
        let cid = &c.change_id[..c.change_id.len().min(12)];
        format!("{sha} {title} ({cid})", title = c.title)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand<'a>(sha: &'a str, title: &'a str, change_id: &'a str) -> Candidate<'a> {
        Candidate {
            commit_sha: sha,
            title,
            change_id,
        }
    }

    #[test]
    fn not_found_is_stack_not_found_with_field_and_prefix() {
        match not_found("Change-Id", "Ibeef") {
            CliError::StackNotFound(msg) => {
                assert_eq!(msg, "no commit found matching Change-Id prefix 'Ibeef'");
            }
            other => panic!("expected StackNotFound, got: {other:?}"),
        }
    }

    #[test]
    fn ambiguous_is_invalid_state_with_count_and_header() {
        let candidates = [
            cand(
                "aaaaaaaaaaaa1111111111111111111111111111",
                "first",
                "Iaaaaaaaaaaaa1111111111111111111111111111",
            ),
            cand(
                "bbbbbbbbbbbb2222222222222222222222222222",
                "second",
                "Ibbbbbbbbbbbb2222222222222222222222222222",
            ),
        ];
        match ambiguous("SHA", "ab", &candidates) {
            CliError::InvalidState(msg) => {
                // 12-char SHA + 12-char Change-Id (the `I` plus 11
                // hex chars).
                assert_eq!(
                    msg,
                    "ambiguous SHA prefix 'ab' matches 2 commits:\n  \
                     aaaaaaaaaaaa first (Iaaaaaaaaaaa)\n  \
                     bbbbbbbbbbbb second (Ibbbbbbbbbbb)"
                );
            }
            other => panic!("expected InvalidState, got: {other:?}"),
        }
    }

    #[test]
    fn ambiguous_truncates_sha_and_change_id_to_twelve() {
        let candidates = [cand(
            "0123456789abcdef0123456789abcdef01234567",
            "subject",
            "I0123456789abcdef0123456789abcdef01234567",
        )];
        match ambiguous("SHA", "01", &candidates) {
            CliError::InvalidState(msg) => {
                // 12-char SHA, 12-char Change-Id (the `I` plus 11
                // hex), not the 7-char SHA / no-Change-Id form the
                // pre-fix code produced.
                assert!(
                    msg.contains("  0123456789ab subject (I0123456789a)"),
                    "got: {msg}"
                );
            }
            other => panic!("expected InvalidState, got: {other:?}"),
        }
    }

    #[test]
    fn ambiguous_omits_parens_when_no_change_id() {
        let candidates = [cand("abcdef0123456789", "no cid", "")];
        match ambiguous("SHA", "abc", &candidates) {
            CliError::InvalidState(msg) => {
                assert!(msg.contains("  abcdef012345 no cid"), "got: {msg}");
                assert!(!msg.contains('('), "no parens expected: {msg}");
            }
            other => panic!("expected InvalidState, got: {other:?}"),
        }
    }

    #[test]
    fn ambiguous_handles_short_sha_and_change_id_without_panicking() {
        // A SHA/Change-Id shorter than 12 chars must clamp rather
        // than slice out of bounds.
        let candidates = [cand("abc", "short", "Ixyz")];
        match ambiguous("Change-Id", "Ix", &candidates) {
            CliError::InvalidState(msg) => {
                assert!(msg.contains("  abc short (Ixyz)"), "got: {msg}");
            }
            other => panic!("expected InvalidState, got: {other:?}"),
        }
    }
}
