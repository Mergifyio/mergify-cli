//! The "this PR is part of a stack" sticky comment Mergify posts
//! on every PR in a stack.
//!
//! Body has three parts:
//!
//! 1. A markdown header pointing at the docs.
//! 2. A 3-column table — `# | Pull Request | Link | 👈` —
//!    listing every PR in the stack, with the row of the PR being
//!    rendered carrying a 👈 emoji.
//! 3. A single-line HTML comment with the marker prefix
//!    `<!-- mergify-stack-data: {…} -->` carrying the same data
//!    as JSON. The marker is what lets `mergify stack checkout`
//!    rebuild the stack from any PR without re-walking GitHub.
//!
//! Ported from `mergify_cli/stack/push.py::StackComment`. Wire
//! shape — header strings, JSON payload, compact one-line marker
//! — is contract: existing comments on every Mergify-managed PR
//! need to parse, and `stack checkout` reads the marker.

use std::fmt::Write;

use serde::Serialize;

/// Markdown header for the current generation of the comment.
/// Match against this to identify a fresh sticky comment posted by
/// the current CLI.
pub const STACK_COMMENT_HEADER: &str =
    "This pull request is part of a [Mergify stack](https://docs.mergify.com/stacks/):\n";

/// Legacy header used by older CLI versions. Kept in
/// [`is_stack_comment`] so an in-place rewrite of an old comment
/// still recognises it as ours instead of leaving a duplicate.
pub const STACK_COMMENT_OLD_HEADER: &str = "This pull request is part of a stack:\n";

const MARKER_PREFIX: &str = "<!-- mergify-stack-data: ";
const MARKER_SUFFIX: &str = " -->";

/// One row in the stack comment + one entry in the JSON marker.
/// Caller materialises this from a `LocalChange` once the PR
/// number and head SHA are known (i.e. after `stack push` has
/// upserted the PR).
#[derive(Debug, Clone)]
pub struct StackEntry {
    pub number: u64,
    pub change_id: String,
    pub head_sha: String,
    pub base_branch: String,
    pub dest_branch: String,
    pub title: String,
    pub html_url: String,
}

/// Render the comment body for the PR identified by
/// `current_number`. `stack_id` goes into the JSON marker only.
#[must_use]
pub fn body(entries: &[StackEntry], current_number: u64, stack_id: &str) -> String {
    let mut out = String::with_capacity(256 + entries.len() * 64);
    out.push_str(STACK_COMMENT_HEADER);
    out.push('\n');
    out.push_str("| # | Pull Request | Link | |\n");
    out.push_str("|--:|---|---|---|\n");

    for (row, entry) in entries.iter().enumerate() {
        let row_num = row + 1;
        // GitHub markdown table cells: `|` is the column
        // separator, so the title's literal pipes must escape.
        let title = entry.title.replace('|', "\\|");
        let status = if entry.number == current_number {
            "👈"
        } else {
            ""
        };
        writeln!(
            out,
            "| {row_num} | {title} | [#{number}]({url}) | {status} |",
            number = entry.number,
            url = entry.html_url,
        )
        .expect("write to String never fails");
    }

    out.push_str(&json_marker(entries, current_number, stack_id));
    out.push('\n');
    out
}

/// True if `comment_body` starts with either the current or the
/// legacy stack-comment header. Used by the upserter to decide
/// "edit this comment" vs "post a new one."
#[must_use]
pub fn is_stack_comment(comment_body: &str) -> bool {
    comment_body.starts_with(STACK_COMMENT_HEADER)
        || comment_body.starts_with(STACK_COMMENT_OLD_HEADER)
}

#[derive(Serialize)]
struct MarkerPull<'a> {
    number: u64,
    change_id: &'a str,
    head_sha: &'a str,
    base_branch: &'a str,
    dest_branch: &'a str,
    is_current: bool,
}

#[derive(Serialize)]
struct MarkerPayload<'a> {
    schema_version: u8,
    stack_id: &'a str,
    pulls: Vec<MarkerPull<'a>>,
}

fn json_marker(entries: &[StackEntry], current_number: u64, stack_id: &str) -> String {
    // `is_current` is a JSON boolean — Python emits the result of
    // `int(...) == current_number` directly, which `json.dumps`
    // serialises as `true`/`false`. Stack-comment readers (incl.
    // `stack checkout`) expect a bool; an integer would break
    // historic-comment parsing.
    let pulls = entries
        .iter()
        .map(|e| MarkerPull {
            number: e.number,
            change_id: &e.change_id,
            head_sha: &e.head_sha,
            base_branch: &e.base_branch,
            dest_branch: &e.dest_branch,
            is_current: e.number == current_number,
        })
        .collect();
    let payload = MarkerPayload {
        schema_version: 1,
        stack_id,
        pulls,
    };
    // serde_json's default formatter is already compact (no
    // whitespace), matching Python's `separators=(",", ":")`.
    let json = serde_json::to_string(&payload).expect("MarkerPayload always serialises");
    format!("{MARKER_PREFIX}{json}{MARKER_SUFFIX}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(number: u64, change_id: &str, head_sha: &str) -> StackEntry {
        StackEntry {
            number,
            change_id: change_id.to_string(),
            head_sha: head_sha.to_string(),
            base_branch: "main".to_string(),
            dest_branch: format!("jd/feature/{}", &change_id[..7]),
            title: format!("feat: change {number}"),
            html_url: format!("https://github.com/o/r/pull/{number}"),
        }
    }

    #[test]
    fn header_marks_current_row_with_pointer() {
        let entries = vec![
            entry(
                1,
                "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "1111111111111111111111111111111111111111",
            ),
            entry(
                2,
                "Ibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "2222222222222222222222222222222222222222",
            ),
        ];
        let out = body(&entries, 2, "feature");
        // The 👈 emoji must land on the row whose `number` equals
        // `current_number` — readers rely on it to find "you are
        // here" in a long stack.
        assert!(out.contains("| 2 | feat: change 2 | [#2](https://github.com/o/r/pull/2) | 👈 |"));
        assert!(out.contains("| 1 | feat: change 1 | [#1](https://github.com/o/r/pull/1) |  |"));
    }

    #[test]
    fn table_cells_escape_literal_pipes_in_title() {
        // A pipe in the title would otherwise close the column
        // and corrupt the GitHub-rendered table.
        let mut e = entry(
            1,
            "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "1111111111111111111111111111111111111111",
        );
        e.title = "feat: support a|b syntax".into();
        let out = body(&[e], 1, "feature");
        assert!(out.contains("feat: support a\\|b syntax"));
    }

    #[test]
    fn json_marker_round_trips_payload_with_is_current_flag() {
        let entries = vec![
            entry(
                1,
                "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "1111111111111111111111111111111111111111",
            ),
            entry(
                2,
                "Ibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "2222222222222222222222222222222222222222",
            ),
        ];
        let out = body(&entries, 1, "feature");

        let marker_line = out
            .lines()
            .find(|l| l.starts_with(MARKER_PREFIX))
            .expect("marker line present");
        assert!(marker_line.ends_with(MARKER_SUFFIX));

        let payload = &marker_line[MARKER_PREFIX.len()..marker_line.len() - MARKER_SUFFIX.len()];
        let parsed: serde_json::Value = serde_json::from_str(payload).unwrap();
        assert_eq!(parsed["schema_version"], 1);
        assert_eq!(parsed["stack_id"], "feature");
        assert_eq!(parsed["pulls"][0]["number"], 1);
        assert_eq!(parsed["pulls"][0]["is_current"], true);
        assert_eq!(parsed["pulls"][1]["number"], 2);
        assert_eq!(parsed["pulls"][1]["is_current"], false);
        // change_id / head_sha / base_branch / dest_branch carry
        // through verbatim — `stack checkout` reads them.
        assert_eq!(
            parsed["pulls"][0]["change_id"],
            "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        );
        assert_eq!(
            parsed["pulls"][1]["head_sha"],
            "2222222222222222222222222222222222222222",
        );
    }

    #[test]
    fn json_marker_is_a_single_compact_line() {
        // Same invariant Python pins: `separators=(",", ":")` —
        // no whitespace between keys/values, no line breaks
        // inside the marker. A multi-line marker would break the
        // single-line readers in `_update_comment_for_pull`.
        let e = entry(
            1,
            "Iaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "1111111111111111111111111111111111111111",
        );
        let out = body(&[e], 1, "feature");
        let marker_line = out.lines().find(|l| l.starts_with(MARKER_PREFIX)).unwrap();
        assert!(!marker_line.contains("\", "));
        assert!(!marker_line.contains("\": "));
    }

    #[test]
    fn is_stack_comment_accepts_current_and_legacy_headers() {
        assert!(is_stack_comment(&format!("{STACK_COMMENT_HEADER}rest")));
        // Pre-docs-link generation; still ours.
        assert!(is_stack_comment(&format!("{STACK_COMMENT_OLD_HEADER}rest")));
        assert!(!is_stack_comment("Some other comment\n"));
        assert!(!is_stack_comment(""));
    }
}
