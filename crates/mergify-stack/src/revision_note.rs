//! The revision-history **git note** — the machine source of
//! truth for a stacked change's revision history, stored on
//! `refs/notes/mergify/stack` against the pushed head commit.
//!
//! Format: a human digest (newest revision first, no compare
//! URLs — they reference SHAs that get GC'd), a blank line, then
//! the same `<!-- mergify-revision-data: {…} -->` marker line the
//! PR comment carries (one schema, owned by
//! [`crate::revision_history`]). The PR comment is a rendering of
//! this data; nothing machine-reads the comment anymore.
//!
//! The note replaces the plain amend-reason note `mergify stack
//! note` wrote on the same commit — the reason is consumed into
//! the new revision entry at push time. Disambiguation rule: a
//! note containing a marker line is a history note; a note
//! without one is a plain reason.
//!
//! `git notes merge --strategy=union` (run by
//! [`crate::notes_push::fetch_notes_ref`]) can concatenate two
//! divergent notes, so [`parse`] scans *all* lines for markers
//! and keeps the payload with the most entries (tie → the later
//! one). The next push rewrites a clean note — self-healing.
//!
//! At merge time the Mergify engine copies this note's blob
//! verbatim onto the merge/squash commit, so the history stays
//! reachable from the base branch forever. The engine never
//! parses the content — the CLI owns the format outright.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use mergify_core::{CliError, HttpClient};
use serde::Deserialize;

use crate::git::run_git_capture;
use crate::local_commits::STACK_NOTES_REF;
use crate::revision_history::{self, RevisionHistoryComment, TIMESTAMP_HUMAN_FMT};

/// Render the full note text for `history`: digest + marker.
#[must_use]
pub fn render(history: &RevisionHistoryComment, pull_number: u64) -> String {
    let mut out = format!("Revision history for #{pull_number}:\n\n");
    for entry in history.entries.iter().rev() {
        // Infallible: write! into a String cannot fail.
        let _ = write!(out, "rev {} ({})", entry.number, entry.change_type.as_str());
        if let Some(ts) = &entry.timestamp {
            let _ = write!(out, " {}", ts.format(TIMESTAMP_HUMAN_FMT));
        }
        if !entry.reason.is_empty() {
            let _ = write!(out, " \u{2014} {}", entry.reason.replace('\n', " "));
        }
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&history.marker_line(pull_number));
    out.push('\n');
    out
}

/// Parse a note back into a history. Returns `None` when no line
/// carries a valid marker — i.e. the note is a plain amend
/// reason (or absent history).
#[must_use]
pub fn parse(
    note_text: &str,
    github_server: &str,
    user: &str,
    repo: &str,
) -> Option<RevisionHistoryComment> {
    let mut best: Option<Vec<revision_history::RevisionEntry>> = None;
    for line in note_text.lines() {
        let Some(payload) = revision_history::parse_marker_line(line) else {
            continue;
        };
        let Some(entries) = revision_history::entries_from_marker(&payload) else {
            continue;
        };
        // `>=` so the later of two equally-long histories wins.
        if best.as_ref().is_none_or(|b| entries.len() >= b.len()) {
            best = Some(entries);
        }
    }
    best.map(|entries| RevisionHistoryComment::from_entries(github_server, user, repo, entries))
}

/// Recover the history a previous (failed) push attempt already
/// wrote on the local head commit. Returns it only when its last
/// entry records exactly the `old_sha` → `new_sha` transition the
/// current push is about to append — anything else (another
/// machine pushed meanwhile, a different amend) means the note is
/// stale and the caller must rebuild from the old head's note.
/// Keeping the recovered entry (reason, timestamp, and all) makes
/// retries idempotent instead of appending a blank-reason
/// duplicate.
#[must_use]
pub fn recover_pending(
    note_text: &str,
    github_server: &str,
    user: &str,
    repo: &str,
    old_sha: &str,
    new_sha: &str,
) -> Option<RevisionHistoryComment> {
    parse(note_text, github_server, user, repo).filter(|h| {
        h.entries
            .last()
            .is_some_and(|e| e.old_sha.as_deref() == Some(old_sha) && e.new_sha == new_sha)
    })
}

/// Read the raw note text on `sha` from the stack notes ref.
/// `None` covers both "no note" and "git failed" — the caller
/// treats both as "no previous history here".
#[must_use]
pub fn read_note(repo_dir: Option<&Path>, sha: &str) -> Option<String> {
    let notes_ref = format!("--ref={STACK_NOTES_REF}");
    let mut cmd = Command::new("git");
    if let Some(dir) = repo_dir {
        cmd.arg("-C").arg(dir);
    }
    let output = cmd.args(["notes", &notes_ref, "show", sha]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Overwrite the note on `sha` with `text` (`git notes add -f`).
pub fn write_note(repo_dir: Option<&Path>, sha: &str, text: &str) -> Result<(), CliError> {
    let notes_ref = format!("--ref={STACK_NOTES_REF}");
    run_git_capture(
        repo_dir,
        &["notes", &notes_ref, "add", "-f", "-m", text, sha],
    )
    .map(|_| ())
}

#[derive(Deserialize)]
struct Comment {
    body: String,
}

/// Load the previous revision history for a change whose old
/// remote head is `old_sha`.
///
/// Read order:
/// 1. History note on `old_sha` (present locally after
///    `fetch_notes_ref`) — the steady state.
/// 2. The PR's revision-history comment — one-time migration
///    seed for stacks last pushed by an older CLI. Converges on
///    first push: the caller writes the note, and this GET never
///    runs again for the change.
/// 3. `None` — first revision of this PR; caller starts fresh
///    with `create_initial`.
pub async fn load_or_seed(
    client: &HttpClient,
    repo_dir: Option<&Path>,
    github_server: &str,
    user: &str,
    repo: &str,
    pull_number: u64,
    old_sha: &str,
) -> Result<Option<RevisionHistoryComment>, CliError> {
    if let Some(text) = read_note(repo_dir, old_sha)
        && let Some(history) = parse(&text, github_server, user, repo)
    {
        return Ok(Some(history));
    }

    let path = format!("/repos/{user}/{repo}/issues/{pull_number}/comments");
    let comments: Vec<Comment> = client.get(&path).await?;
    for comment in &comments {
        if RevisionHistoryComment::is_revision_comment(&comment.body)
            && let Some(parsed) =
                RevisionHistoryComment::parse(&comment.body, github_server, user, repo)
        {
            return Ok(Some(parsed));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use mergify_core::ApiFlavor;
    use serde_json::json;
    use tempfile::TempDir;
    use url::Url;
    use wiremock::matchers::{method, path as wm_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::change_type::ChangeType;

    const GH: &str = "https://api.github.com";

    fn t() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 4, 12, 30, 0).unwrap()
    }

    fn sample_history() -> RevisionHistoryComment {
        let mut h = RevisionHistoryComment::create_initial(
            GH,
            "o",
            "r",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ChangeType::Content,
            t(),
            "first reason",
            None,
        );
        h.append(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "cccccccccccccccccccccccccccccccccccccccc",
            ChangeType::Rebase,
            t(),
            "",
            None,
        );
        h
    }

    #[test]
    fn render_parse_round_trip() {
        let history = sample_history();
        let note = render(&history, 42);
        // Digest is newest-first, human-readable, no URLs. The
        // marker line that follows the digest *does* carry
        // `compare_url` fields (it's the exact same JSON schema
        // the PR comment marker uses), so scope the "no compare
        // URLs" check to the digest portion only.
        assert!(note.starts_with("Revision history for #42:\n\n"));
        assert!(note.contains("rev 3 (rebase)"));
        assert!(note.contains("rev 1 (initial)"));
        assert!(note.contains("\u{2014} first reason"));
        let digest = note.split("<!-- mergify-revision-data:").next().unwrap();
        assert!(!digest.contains("compare"));

        let parsed = parse(&note, GH, "o", "r").expect("round-trips");
        assert_eq!(parsed.entries, history.entries);
    }

    #[test]
    fn parse_plain_reason_returns_none() {
        assert!(parse("fixed a typo in the docs", GH, "o", "r").is_none());
    }

    #[test]
    fn parse_union_merged_note_keeps_longest_history() {
        // Union merge concatenated a 2-entry history with a
        // 3-entry one (order shouldn't matter): the 3-entry
        // payload must win.
        let long = sample_history(); // 3 entries
        let short = RevisionHistoryComment::create_initial(
            GH,
            "o",
            "r",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ChangeType::Content,
            t(),
            "first reason",
            None,
        ); // 2 entries
        let concatenated = format!("{}\n{}", render(&long, 42), render(&short, 42));
        let parsed = parse(&concatenated, GH, "o", "r").expect("parses");
        assert_eq!(parsed.entries.len(), 3);

        let concatenated_rev = format!("{}\n{}", render(&short, 42), render(&long, 42));
        let parsed = parse(&concatenated_rev, GH, "o", "r").expect("parses");
        assert_eq!(parsed.entries.len(), 3);
    }

    #[test]
    fn parse_tolerates_garbage_around_marker() {
        let note = format!(
            "user scribble\n<!-- mergify-revision-data: not json -->\n{}",
            render(&sample_history(), 42),
        );
        let parsed = parse(&note, GH, "o", "r").expect("valid marker still found");
        assert_eq!(parsed.entries.len(), 3);
    }

    #[test]
    fn recover_pending_matches_last_entry_transition() {
        let history = sample_history();
        let note = render(&history, 42);
        let last = history.entries.last().unwrap();
        let recovered = recover_pending(
            &note,
            GH,
            "o",
            "r",
            last.old_sha.as_deref().unwrap(),
            &last.new_sha,
        )
        .expect("recovers matching pending history");
        assert_eq!(recovered.entries, history.entries);
    }

    #[test]
    fn recover_pending_rejects_mismatched_last_entry() {
        let history = sample_history();
        let note = render(&history, 42);
        let last = history.entries.last().unwrap();
        // A different new_sha than what's recorded means another
        // push (or amend) happened since this note was written —
        // the note is stale and must not be reused.
        assert!(
            recover_pending(
                &note,
                GH,
                "o",
                "r",
                last.old_sha.as_deref().unwrap(),
                "ffffffffffffffffffffffffffffffffffffffff",
            )
            .is_none()
        );
    }

    #[test]
    fn recover_pending_rejects_plain_reason() {
        assert!(
            recover_pending(
                "fixed a typo in the docs",
                GH,
                "o",
                "r",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            )
            .is_none()
        );
    }

    // --- git IO ---

    fn init_repo() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@e.com"],
            &["config", "user.name", "T"],
            &["commit", "--allow-empty", "-m", "root"],
        ] {
            let ok = crate::test_env::isolated_git()
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?} failed");
        }
        dir
    }

    fn head_sha(dir: &std::path::Path) -> String {
        run_git_capture(Some(dir), &["rev-parse", "HEAD"]).unwrap()
    }

    #[test]
    fn write_then_read_note_round_trips() {
        let dir = init_repo();
        let sha = head_sha(dir.path());
        let text = render(&sample_history(), 42);
        write_note(Some(dir.path()), &sha, &text).unwrap();
        let read = read_note(Some(dir.path()), &sha).expect("note exists");
        // `git notes show` normalises the trailing newline; compare
        // trimmed.
        assert_eq!(read.trim_end(), text.trim_end());
        // And the full note→history read path works.
        assert!(parse(&read, GH, "o", "r").is_some());
    }

    #[test]
    fn write_note_overwrites_existing_plain_reason() {
        let dir = init_repo();
        let sha = head_sha(dir.path());
        write_note(Some(dir.path()), &sha, "plain reason").unwrap();
        let text = render(&sample_history(), 42);
        write_note(Some(dir.path()), &sha, &text).unwrap();
        let read = read_note(Some(dir.path()), &sha).unwrap();
        assert!(read.contains("mergify-revision-data"));
        assert!(!read.contains("plain reason"));
    }

    #[test]
    fn read_note_missing_returns_none() {
        let dir = init_repo();
        let sha = head_sha(dir.path());
        assert!(read_note(Some(dir.path()), &sha).is_none());
    }

    // --- load_or_seed ---

    fn client(server: &MockServer) -> HttpClient {
        HttpClient::new(
            Url::parse(&server.uri()).unwrap(),
            "token",
            ApiFlavor::GitHub,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn load_prefers_note_over_comment() {
        let dir = init_repo();
        let sha = head_sha(dir.path());
        write_note(Some(dir.path()), &sha, &render(&sample_history(), 42)).unwrap();

        // No comment GET expected: mock with .expect(0).
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(wm_path("/repos/o/r/issues/42/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(0)
            .mount(&server)
            .await;

        let got = load_or_seed(&client(&server), Some(dir.path()), GH, "o", "r", 42, &sha)
            .await
            .unwrap()
            .expect("note found");
        assert_eq!(got.entries.len(), 3);
    }

    #[tokio::test]
    async fn load_seeds_from_comment_when_note_missing() {
        let dir = init_repo();
        let sha = head_sha(dir.path());
        let comment_body = sample_history().body(42);

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(wm_path("/repos/o/r/issues/42/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"url": format!("{}/repos/o/r/issues/comments/1", server.uri()), "body": comment_body},
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let got = load_or_seed(&client(&server), Some(dir.path()), GH, "o", "r", 42, &sha)
            .await
            .unwrap()
            .expect("seeded from comment");
        assert_eq!(got.entries.len(), 3);
    }

    #[tokio::test]
    async fn load_returns_none_when_neither_exists() {
        let dir = init_repo();
        let sha = head_sha(dir.path());
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(wm_path("/repos/o/r/issues/42/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&server)
            .await;

        let got = load_or_seed(&client(&server), Some(dir.path()), GH, "o", "r", 42, &sha)
            .await
            .unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn load_ignores_plain_reason_note_and_falls_through() {
        // Old head carries a plain amend reason (older-CLI state):
        // not a history note → fall through to the comment seed.
        let dir = init_repo();
        let sha = head_sha(dir.path());
        write_note(Some(dir.path()), &sha, "an old plain reason").unwrap();

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(wm_path("/repos/o/r/issues/42/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&server)
            .await;

        let got = load_or_seed(&client(&server), Some(dir.path()), GH, "o", "r", 42, &sha)
            .await
            .unwrap();
        assert!(got.is_none());
    }
}
