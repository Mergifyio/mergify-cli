//! `Change-Id` parsing helpers.
//!
//! Mergify tags each stacked commit with a `Change-Id: I<40-hex>`
//! trailer in the commit message body — that ID is what pins a
//! local commit to its remote branch + PR across rewrites. The
//! Python code mirrored by this module ships three flavours of
//! the same value:
//!
//! - the full I-prefixed 40-hex Change-Id (`I000…aaa`)
//! - a "prefix" that may be partial (typed by the user on the CLI)
//! - an 8-hex "short" form embedded in the new-style branch name
//!   suffix `…--abcd1234`
//!
//! Extraction from a commit body uses a deliberately permissive
//! regex (`[0-9a-z]` not `[0-9a-f]`) so a malformed trailer still
//! surfaces and `is_full` can flag it; validation is strict.

use std::sync::LazyLock;

use regex::Regex;

/// Permissive pattern used when *finding* a Change-Id trailer in a
/// commit body. Stays loose (`[0-9a-z]`, not `[0-9a-f]`) so a
/// malformed `Change-Id: Ixxxg…` still gets pulled out, lets the
/// caller see what was written instead of silently returning
/// `None`.
const CHANGEID_LOOSE_PATTERN: &str = r"I[0-9a-z]{40}";

/// Strict full-form pattern: I + 40 lowercase hex chars.
const CHANGEID_FULL_PATTERN: &str = r"^I[0-9a-f]{40}$";

/// Suffix pattern: `--<8 hex>` at end of a string. Used to peel
/// the short Change-Id out of new-style branch segments.
const SHORT_CHANGEID_PATTERN: &str = r"--([0-9a-f]{8})$";

static CHANGEID_TRAILER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"Change-Id: ({CHANGEID_LOOSE_PATTERN})")).unwrap());

static CHANGEID_FULL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(CHANGEID_FULL_PATTERN).unwrap());

static SHORT_CHANGEID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(SHORT_CHANGEID_PATTERN).unwrap());

/// Branch-suffix pattern: `/I<40-hex>` at end of a branch name —
/// the legacy stack-branch naming scheme. Used by
/// `stack checkout` to strip the suffix from the user-supplied
/// stack name when the user passes a full leaf-branch ref.
static BRANCH_SUFFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"/{CHANGEID_LOOSE_PATTERN}$")).unwrap());

/// Strip a trailing `/Ixxxx…` Change-Id suffix from a branch
/// name. Returns the input unchanged when there's no match.
#[must_use]
pub fn strip_branch_suffix(name: &str) -> String {
    BRANCH_SUFFIX_RE.replace(name, "").into_owned()
}

/// Return `true` if `value` is a strict, full-form Change-Id
/// (`I` + 40 lowercase hex chars).
#[must_use]
pub fn is_full(value: &str) -> bool {
    CHANGEID_FULL_RE.is_match(value)
}

/// Return `true` if `prefix` could be the start of a Change-Id.
/// Requires at least two characters (the leading `I` plus one hex
/// digit) so a bare `"I"` doesn't qualify. Used by the CLI to let
/// users target a stack commit by typing only the first few
/// characters of its Change-Id.
#[must_use]
pub fn is_prefix(prefix: &str) -> bool {
    let bytes = prefix.as_bytes();
    bytes.len() >= 2
        && bytes[0] == b'I'
        && bytes[1..]
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
}

/// Extract the *last* `Change-Id: …` trailer from a commit
/// message body. Returns `None` if no trailer is present. The
/// returned value is the raw match — callers that want strictness
/// should pass it through [`is_full`].
///
/// "Last" matters: Mergify rewrites tend to append a new trailer
/// rather than replace, and the most recent one wins.
#[must_use]
pub fn extract_from_message(message: &str) -> Option<&str> {
    CHANGEID_TRAILER_RE
        .captures_iter(message)
        .last()
        .map(|cap| cap.get(1).unwrap().as_str())
}

/// Extract a Change-Id from the last segment of a stack branch
/// name. Returns either:
///
/// - the **full** Change-Id (old-style branch, the segment *is*
///   a Change-Id, 41 chars `I…`), or
/// - the **short** 8-hex tail (new-style branch, segment ends in
///   `--xxxxxxxx`).
///
/// Callers need both because remote branches in the wild can be
/// either shape. The matching layer ([`crate::change_id`]
/// callers in `changes.py`) handles the cross-format pairing.
#[must_use]
pub fn extract_from_branch_segment(segment: &str) -> Option<&str> {
    if is_full(segment) {
        return Some(segment);
    }
    SHORT_CHANGEID_RE
        .captures(segment)
        .map(|cap| cap.get(1).unwrap().as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_full_accepts_well_formed_id() {
        let id = "I0123456789abcdef0123456789abcdef01234567";
        assert!(is_full(id));
    }

    #[test]
    fn is_full_rejects_uppercase_hex_and_wrong_length_and_wrong_prefix() {
        // Hex chars must be lowercase.
        assert!(!is_full("I0123456789ABCDEF0123456789abcdef01234567"));
        // Off-by-one — 39 hex.
        assert!(!is_full("I0123456789abcdef0123456789abcdef0123456"));
        // Off-by-one — 41 hex.
        assert!(!is_full("I0123456789abcdef0123456789abcdef012345678"));
        // Missing leading I.
        assert!(!is_full("0123456789abcdef0123456789abcdef01234567"));
        // Empty.
        assert!(!is_full(""));
    }

    #[test]
    fn is_prefix_accepts_partial_id_with_at_least_one_hex() {
        assert!(is_prefix("Ia"));
        assert!(is_prefix("I0"));
        assert!(is_prefix("Iabc123"));
    }

    #[test]
    fn is_prefix_rejects_bare_i_and_non_hex_chars() {
        // Bare "I" — Python requires at least two chars so a stray
        // capital I doesn't accidentally match.
        assert!(!is_prefix("I"));
        // Non-hex after the I.
        assert!(!is_prefix("Ig"));
        assert!(!is_prefix("Iaz"));
        // Empty.
        assert!(!is_prefix(""));
        // No leading I.
        assert!(!is_prefix("abc"));
    }

    #[test]
    fn extract_from_message_returns_last_trailer_when_multiple() {
        // Commits get rewritten and amends sometimes append a new
        // trailer instead of replacing — the rightmost one wins.
        let msg = "subject\n\nChange-Id: Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nChange-Id: Ibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n";
        assert_eq!(
            extract_from_message(msg),
            Some("Ibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        );
    }

    #[test]
    fn extract_from_message_returns_none_when_missing() {
        assert_eq!(extract_from_message("subject\n\nbody"), None);
    }

    #[test]
    fn extract_from_message_uses_loose_pattern_so_callers_can_flag_typos() {
        // `g` is outside [0-9a-f] but inside [0-9a-z]. The
        // extractor still surfaces it so the caller can run it
        // through `is_full` and report "looks like a Change-Id but
        // isn't valid hex". Silently dropping it would make the
        // error message useless.
        let msg = "Change-Id: Igggggggggggggggggggggggggggggggggggggggg";
        let extracted = extract_from_message(msg).expect("loose match");
        assert!(!is_full(extracted));
    }

    #[test]
    fn extract_from_branch_segment_returns_full_id_for_old_style() {
        let id = "I0123456789abcdef0123456789abcdef01234567";
        assert_eq!(extract_from_branch_segment(id), Some(id));
    }

    #[test]
    fn extract_from_branch_segment_returns_short_tail_for_new_style() {
        // `mergify stack push` slugifies the commit title and
        // appends `--<8 hex>` for human readability.
        assert_eq!(
            extract_from_branch_segment("my-slug--abcd1234"),
            Some("abcd1234"),
        );
    }

    #[test]
    fn extract_from_branch_segment_returns_none_for_unrelated_segments() {
        assert_eq!(extract_from_branch_segment(""), None);
        assert_eq!(extract_from_branch_segment("plain-segment"), None);
        // Has the right shape but wrong length on the short tail.
        assert_eq!(extract_from_branch_segment("slug--abcd123"), None);
        assert_eq!(extract_from_branch_segment("slug--abcd12345"), None);
    }
}
