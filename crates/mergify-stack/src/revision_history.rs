//! The "Revision history" sticky comment posted on every PR
//! `mergify stack push` updates.
//!
//! Each revision adds one table row pinning *what changed*
//! (`rebase` / `content` / `unknown`) and *why* (the amend
//! `reason` attached via `mergify stack note`). The body has
//! three parts:
//!
//! 1. A fixed first-line header.
//! 2. A markdown table with one row per revision.
//! 3. A single-line `<!-- mergify-revision-data: {…} -->`
//!    marker that carries the *machine-readable* version of the
//!    same data, including the SHAs the table-cell rendering
//!    only shows in truncated form. `parse()` reads the marker
//!    back so subsequent pushes can append instead of rewriting.
//!
//! Ported from `mergify_cli/stack/push.py::RevisionHistoryComment`.
//! Wire shape — table format, marker JSON, ISO-8601 timestamps
//! — is contract: live PRs carry historic comments produced by
//! the Python implementation and `parse()` has to round-trip
//! them.
//!
//! Compare URLs: when a `replay_sha` is available (synthetic
//! commit whose tree is the user's amendment isolated from
//! rebase noise — see [`crate::replay`]) the row's compare URL
//! anchors at it for the headline link and falls back to the
//! raw `old→new` URL in a parenthetical `(raw)` for when the
//! synth commit gets GC'd by GitHub. Both URLs use `old_sha`
//! as the merge-base for the diff.

use std::fmt::Write;
use std::sync::LazyLock;

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::change_type::ChangeType;

/// First-line header used to recognise our sticky comment. The
/// trailing `\n` is part of the contract — the matcher uses
/// `starts_with`, so a body that opens with `### Revision
/// history` *followed by other content on the same line* is
/// **not** ours.
pub const REVISION_COMMENT_FIRST_LINE: &str = "### Revision history\n";

const MARKER_PREFIX: &str = "<!-- mergify-revision-data: ";
const MARKER_SUFFIX: &str = " -->";

/// Soft cap on the reason cell. Long single-line reasons get an
/// ellipsis to keep the table readable on GitHub. Python uses 200;
/// keep the same number so re-renders of historic rows match.
const MAX_REASON_LEN: usize = 200;

const TIMESTAMP_HUMAN_FMT: &str = "%Y-%m-%d %H:%M UTC";
const TIMESTAMP_ISO_FMT: &str = "%Y-%m-%dT%H:%M:%SZ";

/// Escape `reason` for a single markdown table cell.
///
/// - `\` → `\\` (must come first to avoid double-escaping the
///   next two replacements)
/// - `|` → `\|` (column separator)
/// - `\n` → `<br>` (cells can't span lines on GitHub)
/// - Truncate to [`MAX_REASON_LEN`] with a trailing `…`
///
/// Empty input short-circuits to `""` so the cell is visually
/// empty rather than carrying any placeholder.
#[must_use]
pub fn escape_reason(reason: &str) -> String {
    if reason.is_empty() {
        return String::new();
    }
    let escaped = reason
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace('\n', "<br>");
    if escaped.chars().count() > MAX_REASON_LEN {
        let mut out: String = escaped.chars().take(MAX_REASON_LEN - 1).collect();
        out.push('…');
        out
    } else {
        escaped
    }
}

/// One row in the revision-history table plus one entry in the
/// JSON marker. `old_sha` is `None` only for the synthetic
/// `initial` row that anchors the first PR head (no prior SHA to
/// compare against).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionEntry {
    pub number: u32,
    pub change_type: ChangeType,
    pub old_sha: Option<String>,
    pub new_sha: String,
    pub timestamp: Option<DateTime<Utc>>,
    pub reason: String,
    /// Synth commit from [`crate::replay`]. When present, used
    /// for the headline compare URL so the diff renders the
    /// user's amendment without rebase noise; the raw
    /// `old→new` URL still gets emitted as the durable
    /// fallback.
    pub replay_sha: Option<String>,
}

impl RevisionEntry {
    fn timestamp_human(&self) -> String {
        self.timestamp
            .as_ref()
            .map_or_else(String::new, |t| t.format(TIMESTAMP_HUMAN_FMT).to_string())
    }

    fn timestamp_iso(&self) -> Option<String> {
        self.timestamp
            .as_ref()
            .map(|t| t.format(TIMESTAMP_ISO_FMT).to_string())
    }
}

/// The whole revision-history sticky comment for one PR.
///
/// Build via [`Self::create_initial`] for a fresh comment, or via
/// [`Self::parse`] when re-rendering an existing one (which
/// preserves the original markdown rows verbatim — useful so a
/// re-render of a historic table doesn't whiplash old links).
#[derive(Debug, Clone)]
pub struct RevisionHistoryComment {
    pub github_server: String,
    pub user: String,
    pub repo: String,
    pub entries: Vec<RevisionEntry>,
    /// Original raw rows from a [`Self::parse`]d comment, one
    /// per parsed entry. Rendering uses these verbatim when
    /// present so historic links stay intact across re-renders.
    raw_rows: Vec<String>,
}

impl RevisionHistoryComment {
    /// True if `comment_body` looks like our revision-history
    /// sticky comment.
    #[must_use]
    pub fn is_revision_comment(comment_body: &str) -> bool {
        comment_body.starts_with(REVISION_COMMENT_FIRST_LINE)
    }

    /// Build the initial 2-row comment: a synthetic `initial`
    /// row anchoring `old_sha` and the first real revision row
    /// for the `old → new` change.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn create_initial(
        github_server: &str,
        user: &str,
        repo: &str,
        old_sha: &str,
        new_sha: &str,
        change_type: ChangeType,
        timestamp: DateTime<Utc>,
        reason: &str,
        replay_sha: Option<&str>,
    ) -> Self {
        let entries = vec![
            RevisionEntry {
                number: 1,
                change_type: ChangeType::Initial,
                old_sha: None,
                new_sha: old_sha.to_string(),
                timestamp: Some(timestamp),
                reason: String::new(),
                replay_sha: None,
            },
            RevisionEntry {
                number: 2,
                change_type,
                old_sha: Some(old_sha.to_string()),
                new_sha: new_sha.to_string(),
                timestamp: Some(timestamp),
                reason: reason.to_string(),
                replay_sha: replay_sha.map(str::to_owned),
            },
        ];
        Self {
            github_server: github_server.to_string(),
            user: user.to_string(),
            repo: repo.to_string(),
            entries,
            raw_rows: Vec::new(),
        }
    }

    /// Append a revision row. Number is auto-assigned.
    pub fn append(
        &mut self,
        old_sha: &str,
        new_sha: &str,
        change_type: ChangeType,
        timestamp: DateTime<Utc>,
        reason: &str,
        replay_sha: Option<&str>,
    ) {
        let next_number = u32::try_from(self.entries.len() + 1).unwrap_or(u32::MAX);
        self.entries.push(RevisionEntry {
            number: next_number,
            change_type,
            old_sha: Some(old_sha.to_string()),
            new_sha: new_sha.to_string(),
            timestamp: Some(timestamp),
            reason: reason.to_string(),
            replay_sha: replay_sha.map(str::to_owned),
        });
    }

    fn compare_url(&self, old_sha: &str, new_sha: &str) -> String {
        let api_url = self.github_server.trim_end_matches('/');
        let html_url = if api_url.contains("/api/v3") {
            api_url.replace("/api/v3", "")
        } else {
            api_url.replace("api.github.com", "github.com")
        };
        format!(
            "{html_url}/{user}/{repo}/compare/{old_sha}...{new_sha}",
            user = self.user,
            repo = self.repo,
        )
    }

    fn entry_head_sha(entry: &RevisionEntry) -> &str {
        entry.replay_sha.as_deref().unwrap_or(&entry.new_sha)
    }

    fn render_entry(&self, entry: &RevisionEntry) -> String {
        let changes_cell = match (&entry.old_sha, entry.change_type) {
            (None, _) => format!("`{}`", short_sha(&entry.new_sha)),
            (Some(old), ChangeType::Rebase) => {
                // Patch-id matched — no semantic edit, so no
                // compare URL (it would just show rebase noise).
                format!(
                    "`{} \u{2192} {}` _(rebase only)_",
                    short_sha(old),
                    short_sha(&entry.new_sha),
                )
            }
            (Some(old), _) => {
                let url = self.compare_url(old, Self::entry_head_sha(entry));
                let mut cell = format!(
                    "[`{} \u{2192} {}`]({url})",
                    short_sha(old),
                    short_sha(&entry.new_sha),
                );
                if entry.replay_sha.is_some() {
                    // Synth replay commits get GC'd by GitHub
                    // eventually; pair with the always-resolvable
                    // raw `old→new` URL so the row stays useful
                    // long-term.
                    let raw_url = self.compare_url(old, &entry.new_sha);
                    write!(cell, " ([raw]({raw_url}))").expect("string");
                }
                cell
            }
        };
        format!(
            "| {} | {} | {} | {} | {} |",
            entry.number,
            entry.change_type.as_str(),
            changes_cell,
            escape_reason(&entry.reason),
            entry.timestamp_human(),
        )
    }

    fn json_marker(&self, pull_number: u64) -> String {
        let entries: Vec<MarkerEntry<'_>> = self
            .entries
            .iter()
            .map(|e| MarkerEntry {
                number: e.number,
                change_type: e.change_type.as_str(),
                old_sha: e.old_sha.as_deref(),
                new_sha: &e.new_sha,
                timestamp_iso: e.timestamp_iso(),
                reason: &e.reason,
                replay_sha: e.replay_sha.as_deref(),
                compare_url: e
                    .old_sha
                    .as_deref()
                    .map(|old| self.compare_url(old, Self::entry_head_sha(e))),
            })
            .collect();
        let payload = MarkerPayload {
            schema_version: 1,
            pull_number,
            entries,
        };
        let json = serde_json::to_string(&payload).expect("MarkerPayload always serialises");
        format!("{MARKER_PREFIX}{json}{MARKER_SUFFIX}")
    }

    /// Render the full comment body for `pull_number`.
    ///
    /// Rows preserved from [`Self::parse`] are emitted verbatim
    /// so historic links don't change shape on a re-render. New
    /// rows (appended via [`Self::append`] or seeded by
    /// [`Self::create_initial`]) get rendered fresh.
    #[must_use]
    pub fn body(&self, pull_number: u64) -> String {
        let mut out = String::with_capacity(256 + self.entries.len() * 96);
        out.push_str(REVISION_COMMENT_FIRST_LINE);
        // Python's body builds `"\n".join([header, …])` where the
        // header already has a trailing `\n`, producing a blank
        // line between header and table. Reproduce that gap so
        // re-renders of historic comments don't shift bytes.
        out.push('\n');
        out.push_str("| # | Type | Changes | Reason | Date |\n");
        out.push_str("|---|------|---------|--------|------|\n");
        for (i, entry) in self.entries.iter().enumerate() {
            if let Some(row) = self.raw_rows.get(i) {
                out.push_str(row);
            } else {
                out.push_str(&self.render_entry(entry));
            }
            out.push('\n');
        }
        out.push_str(&self.json_marker(pull_number));
        out.push('\n');
        out
    }

    /// Parse an existing revision-history comment back into a
    /// `RevisionHistoryComment`. Returns `None` if `body` isn't
    /// shaped like our sticky comment.
    ///
    /// The table rows alone don't carry SHAs (they're truncated
    /// for readability); the JSON marker does. Parsing walks
    /// rows first to seed the entry list with `number` +
    /// `change_type` + parsed timestamp, then overlays the
    /// marker payload onto the same entries so old/new SHAs,
    /// reasons, and replay SHAs come through.
    #[must_use]
    pub fn parse(body: &str, github_server: &str, user: &str, repo: &str) -> Option<Self> {
        if !body.starts_with(REVISION_COMMENT_FIRST_LINE) {
            return None;
        }

        let mut entries: Vec<RevisionEntry> = Vec::new();
        let mut raw_rows: Vec<String> = Vec::new();
        let mut marker_entries: Option<Vec<serde_json::Value>> = None;
        // A line that *starts* with the marker prefix but fails
        // to round-trip is corruption, not absence — the
        // upserter recovers from `None` but happily PATCHes a
        // `Some(_)` with empty SHAs, losing the historical
        // payload. Track it separately so we can short-circuit.
        let mut marker_corrupted = false;

        for line in body.lines() {
            // Try the 5-column shape first (current rendering),
            // then fall back to the 4-column legacy shape — old
            // comments out in the wild still need to round-trip.
            if let Some((number, change_type, timestamp)) = match_row_5(line) {
                entries.push(RevisionEntry {
                    number,
                    change_type,
                    old_sha: None,
                    new_sha: String::new(),
                    timestamp,
                    reason: String::new(),
                    replay_sha: None,
                });
                raw_rows.push(line.to_string());
                continue;
            }
            if let Some((number, change_type, timestamp)) = match_row_4(line) {
                entries.push(RevisionEntry {
                    number,
                    change_type,
                    old_sha: None,
                    new_sha: String::new(),
                    timestamp,
                    reason: String::new(),
                    replay_sha: None,
                });
                raw_rows.push(line.to_string());
                continue;
            }
            if let Some(payload) = parse_marker_line(line) {
                if let Some(v) = payload.get("schema_version")
                    && v.as_u64() == Some(1)
                    && let Some(arr) = payload.get("entries").and_then(|v| v.as_array())
                {
                    marker_entries = Some(arr.clone());
                } else {
                    marker_corrupted = true;
                }
            } else if line.starts_with(MARKER_PREFIX) {
                marker_corrupted = true;
            }
        }

        if entries.is_empty() || marker_corrupted {
            return None;
        }

        if let Some(marker) = marker_entries
            && marker.len() == entries.len()
        {
            for (entry, data) in entries.iter_mut().zip(marker.iter()) {
                let Some(obj) = data.as_object() else {
                    continue;
                };
                if obj.get("number").and_then(serde_json::Value::as_u64)
                    != Some(u64::from(entry.number))
                {
                    continue;
                }
                match obj.get("old_sha") {
                    Some(serde_json::Value::String(s)) => entry.old_sha = Some(s.clone()),
                    Some(serde_json::Value::Null) => entry.old_sha = None,
                    _ => {}
                }
                if let Some(serde_json::Value::String(s)) = obj.get("new_sha") {
                    entry.new_sha.clone_from(s);
                }
                match obj.get("timestamp_iso") {
                    Some(serde_json::Value::String(s)) => {
                        if let Ok(naive) = NaiveDateTime::parse_from_str(s, TIMESTAMP_ISO_FMT) {
                            entry.timestamp = Some(Utc.from_utc_datetime(&naive));
                        }
                    }
                    Some(serde_json::Value::Null) => entry.timestamp = None,
                    _ => {}
                }
                if let Some(serde_json::Value::String(s)) = obj.get("reason") {
                    entry.reason.clone_from(s);
                }
                match obj.get("replay_sha") {
                    Some(serde_json::Value::String(s)) => entry.replay_sha = Some(s.clone()),
                    Some(serde_json::Value::Null) => entry.replay_sha = None,
                    _ => {}
                }
            }
        }

        Some(Self {
            github_server: github_server.to_string(),
            user: user.to_string(),
            repo: repo.to_string(),
            entries,
            raw_rows,
        })
    }
}

fn short_sha(sha: &str) -> &str {
    if sha.len() > 7 { &sha[..7] } else { sha }
}

/// 5-column row regex: `| n | type | changes | reason | date |`.
/// `reason` is captured but the parsed entry leaves it empty —
/// the marker is the source of truth for the unescaped reason.
static ROW_RE_5: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\| (\d+) \| (\w+) \| .+ \| (.*) \| (.+) \|$").unwrap());

/// 4-column legacy row: `| n | type | changes | date |`. Older
/// comments lack the reason column.
static ROW_RE_4: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\| (\d+) \| (\w+) \| .+ \| (.+) \|$").unwrap());

static MARKER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"^{}(?P<payload>\{{.*\}}){}$",
        regex::escape(MARKER_PREFIX),
        regex::escape(MARKER_SUFFIX),
    ))
    .unwrap()
});

fn match_row_5(line: &str) -> Option<(u32, ChangeType, Option<DateTime<Utc>>)> {
    let caps = ROW_RE_5.captures(line)?;
    let number = caps.get(1)?.as_str().parse().ok()?;
    let change_type = ChangeType::from_str_lossy(caps.get(2)?.as_str());
    let timestamp = parse_human_timestamp(caps.get(4)?.as_str().trim());
    Some((number, change_type, timestamp))
}

fn match_row_4(line: &str) -> Option<(u32, ChangeType, Option<DateTime<Utc>>)> {
    let caps = ROW_RE_4.captures(line)?;
    let number = caps.get(1)?.as_str().parse().ok()?;
    let change_type = ChangeType::from_str_lossy(caps.get(2)?.as_str());
    let timestamp = parse_human_timestamp(caps.get(3)?.as_str().trim());
    Some((number, change_type, timestamp))
}

fn parse_human_timestamp(s: &str) -> Option<DateTime<Utc>> {
    let naive = NaiveDateTime::parse_from_str(s, TIMESTAMP_HUMAN_FMT).ok()?;
    Some(Utc.from_utc_datetime(&naive))
}

fn parse_marker_line(line: &str) -> Option<serde_json::Value> {
    let caps = MARKER_RE.captures(line)?;
    serde_json::from_str(caps.name("payload")?.as_str()).ok()
}

#[derive(Serialize, Deserialize)]
struct MarkerEntry<'a> {
    number: u32,
    change_type: &'a str,
    old_sha: Option<&'a str>,
    new_sha: &'a str,
    timestamp_iso: Option<String>,
    reason: &'a str,
    replay_sha: Option<&'a str>,
    compare_url: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct MarkerPayload<'a> {
    schema_version: u8,
    pull_number: u64,
    #[serde(borrow)]
    entries: Vec<MarkerEntry<'a>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 4, 12, 30, 0).unwrap()
    }

    #[test]
    fn escape_reason_handles_backslash_pipe_newline_and_truncation() {
        assert_eq!(escape_reason(""), "");
        assert_eq!(escape_reason("a|b"), "a\\|b");
        assert_eq!(escape_reason("a\nb"), "a<br>b");
        // Backslash must be doubled FIRST so the pipe/newline
        // replacements don't double-escape it.
        assert_eq!(escape_reason("a\\b"), "a\\\\b");
        // Truncation: MAX_REASON_LEN - 1 chars + "…".
        let long = "x".repeat(MAX_REASON_LEN + 50);
        let out = escape_reason(&long);
        assert_eq!(out.chars().count(), MAX_REASON_LEN);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn compare_url_strips_api_v3_for_ghe_and_api_dot_github_dot_com_for_dotcom() {
        let c = RevisionHistoryComment {
            github_server: "https://api.github.com".into(),
            user: "o".into(),
            repo: "r".into(),
            entries: Vec::new(),
            raw_rows: Vec::new(),
        };
        assert_eq!(
            c.compare_url("aaa", "bbb"),
            "https://github.com/o/r/compare/aaa...bbb",
        );

        let ghe = RevisionHistoryComment {
            github_server: "https://ghe.example.com/api/v3".into(),
            user: "o".into(),
            repo: "r".into(),
            entries: Vec::new(),
            raw_rows: Vec::new(),
        };
        assert_eq!(
            ghe.compare_url("aaa", "bbb"),
            "https://ghe.example.com/o/r/compare/aaa...bbb",
        );

        // Trailing slash on the API URL must not double up.
        let trailing = RevisionHistoryComment {
            github_server: "https://api.github.com/".into(),
            user: "o".into(),
            repo: "r".into(),
            entries: Vec::new(),
            raw_rows: Vec::new(),
        };
        assert_eq!(
            trailing.compare_url("aaa", "bbb"),
            "https://github.com/o/r/compare/aaa...bbb",
        );
    }

    #[test]
    fn body_renders_initial_and_rebase_and_content_rows() {
        let mut c = RevisionHistoryComment::create_initial(
            "https://api.github.com",
            "o",
            "r",
            "1234567abcdef",
            "abcdef1234567",
            ChangeType::Content,
            t(),
            "review feedback",
            None,
        );
        c.append(
            "abcdef1234567",
            "fedcba7654321",
            ChangeType::Rebase,
            t(),
            "",
            None,
        );

        let body = c.body(42);

        // Initial row: bare short SHA, no compare link.
        assert!(body.contains("| 1 | initial | `1234567` |  | 2026-06-04 12:30 UTC |"));
        // Content row: link to compare URL using old → new.
        assert!(body.contains(
            "| 2 | content | [`1234567 \u{2192} abcdef1`](https://github.com/o/r/compare/1234567abcdef...abcdef1234567) | review feedback | 2026-06-04 12:30 UTC |"
        ));
        // Rebase row: no URL — patch-id matched so the diff
        // would be empty/noise.
        assert!(body.contains(
            "| 3 | rebase | `abcdef1 \u{2192} fedcba7` _(rebase only)_ |  | 2026-06-04 12:30 UTC |"
        ));
    }

    #[test]
    fn body_replay_sha_is_used_for_headline_url_with_raw_fallback() {
        // A row with `replay_sha` set must use it for the
        // headline `[old → new](url)` link (so the compare
        // renders just the amendment) AND surface the raw
        // `old → new` URL in a `(raw)` parenthetical for the
        // long-term-durable fallback.
        let c = RevisionHistoryComment::create_initial(
            "https://api.github.com",
            "o",
            "r",
            "1234567abcdef",
            "abcdef1234567",
            ChangeType::Content,
            t(),
            "edit",
            Some("ffffffffffffff"),
        );
        let body = c.body(42);
        assert!(body.contains(
            "[`1234567 \u{2192} abcdef1`](https://github.com/o/r/compare/1234567abcdef...ffffffffffffff) ([raw](https://github.com/o/r/compare/1234567abcdef...abcdef1234567))"
        ));
    }

    #[test]
    fn json_marker_contains_compare_url_and_replay_sha_for_each_entry() {
        let c = RevisionHistoryComment::create_initial(
            "https://api.github.com",
            "o",
            "r",
            "1234567abcdef",
            "abcdef1234567",
            ChangeType::Content,
            t(),
            "edit",
            Some("ffffffffffffff"),
        );
        let body = c.body(42);
        let line = body
            .lines()
            .find(|l| l.starts_with(MARKER_PREFIX))
            .expect("marker");
        assert!(line.ends_with(MARKER_SUFFIX));
        let payload = &line[MARKER_PREFIX.len()..line.len() - MARKER_SUFFIX.len()];
        let v: serde_json::Value = serde_json::from_str(payload).unwrap();
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["pull_number"], 42);
        assert_eq!(v["entries"][0]["change_type"], "initial");
        assert_eq!(v["entries"][0]["old_sha"], serde_json::Value::Null);
        assert_eq!(v["entries"][0]["compare_url"], serde_json::Value::Null);
        assert_eq!(v["entries"][1]["change_type"], "content");
        assert_eq!(v["entries"][1]["replay_sha"], "ffffffffffffff");
        // compare_url in the marker uses replay_sha when present,
        // matching the headline link.
        assert_eq!(
            v["entries"][1]["compare_url"],
            "https://github.com/o/r/compare/1234567abcdef...ffffffffffffff",
        );
        assert_eq!(v["entries"][1]["timestamp_iso"], "2026-06-04T12:30:00Z");
    }

    #[test]
    fn is_revision_comment_matches_first_line_only() {
        assert!(RevisionHistoryComment::is_revision_comment(
            "### Revision history\nbody",
        ));
        assert!(!RevisionHistoryComment::is_revision_comment(
            "### Revision history details", // not the literal header
        ));
        assert!(!RevisionHistoryComment::is_revision_comment(
            "Some other comment",
        ));
    }

    #[test]
    fn parse_round_trips_a_rendered_body() {
        // Build, render, parse → the parsed instance must
        // produce the same body bytes.
        let mut c = RevisionHistoryComment::create_initial(
            "https://api.github.com",
            "o",
            "r",
            "1234567abcdef",
            "abcdef1234567",
            ChangeType::Content,
            t(),
            "fix lint",
            None,
        );
        c.append(
            "abcdef1234567",
            "fedcba7654321",
            ChangeType::Rebase,
            t(),
            "",
            None,
        );
        let body = c.body(42);

        let parsed = RevisionHistoryComment::parse(&body, "https://api.github.com", "o", "r")
            .expect("parses back");
        assert_eq!(parsed.entries.len(), 3);
        // Entries carry the marker-sourced SHAs + reasons +
        // timestamps.
        assert_eq!(parsed.entries[0].old_sha, None);
        assert_eq!(parsed.entries[0].new_sha, "1234567abcdef");
        assert_eq!(parsed.entries[1].reason, "fix lint");
        assert_eq!(parsed.entries[1].change_type, ChangeType::Content);
        assert_eq!(parsed.entries[2].change_type, ChangeType::Rebase);

        // Re-render preserves the original rows byte-for-byte.
        // Round-trip stability matters: future pushes that add
        // a new row mustn't whiplash old links.
        let rerendered = parsed.body(42);
        assert_eq!(rerendered, body);
    }

    #[test]
    fn parse_returns_none_for_non_revision_comments() {
        assert!(
            RevisionHistoryComment::parse(
                "Some other comment body",
                "https://api.github.com",
                "o",
                "r",
            )
            .is_none()
        );
    }

    #[test]
    fn parse_returns_none_when_marker_line_is_corrupted() {
        // A line that *starts* with the marker prefix but fails
        // to parse (truncated JSON, hand-edited payload, etc.)
        // must be reported as corruption so the upserter's
        // recovery path (overwrite with a fresh initial comment)
        // can fire — otherwise the next render would emit a new
        // marker built from rows with empty `old_sha/new_sha`
        // and the historical SHA payload would be lost.
        let body = "### Revision history\n\
                    | # | Type | Changes | Date | Reason |\n\
                    |---|------|---------|------|--------|\n\
                    | 1 | initial | `1234567` | 2026-06-04 12:30 UTC | |\n\
                    <!-- mergify-revision-data: {\"schema_version\": -->\n";
        assert!(RevisionHistoryComment::parse(body, "https://api.github.com", "o", "r").is_none());
    }

    #[test]
    fn parse_handles_4_column_legacy_rows_when_marker_missing() {
        // Old comments out in the wild have 4 columns (no
        // reason cell). Parsing must still seed the table rows
        // even without a marker so a subsequent append can
        // render a fresh 5-column row underneath them.
        let body = "### Revision history\n\
                    | # | Type | Changes | Date |\n\
                    |---|------|---------|------|\n\
                    | 1 | initial | `1234567` | 2026-06-04 12:30 UTC |\n\
                    | 2 | content | `1234567 \u{2192} abcdef1` | 2026-06-04 12:30 UTC |\n";
        let parsed = RevisionHistoryComment::parse(body, "https://api.github.com", "o", "r")
            .expect("parses");
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(parsed.entries[0].change_type, ChangeType::Initial);
        assert_eq!(parsed.entries[1].change_type, ChangeType::Content);
        // No marker so SHAs stay empty on the parsed entries —
        // the raw rows still render verbatim when re-emitted.
        assert!(parsed.entries[0].new_sha.is_empty());
    }
}
