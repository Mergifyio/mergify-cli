//! Live, in-place progress display for `stack push`.
//!
//! `stack push` runs a sequence of slow network steps — git push,
//! one PR upsert per stacked commit, stack comments, revision
//! history. Done silently it looks frozen, especially on a slow
//! link. [`Progress`] renders one row per step and rewrites those
//! rows in place (spinner → ✓) as each completes, so the terminal
//! always shows what is happening *now*.
//!
//! On anything that is not an interactive terminal (a pipe, CI,
//! a redirect) it degrades to plain streaming: each row is printed
//! once, when it resolves, with no cursor control or color. Tests
//! and scripts therefore see deterministic line-per-step output.
//!
//! The full rendered text is also collected into a transcript
//! (returned by [`Progress::finish`]) so callers can keep the
//! buffered-lines contract the dry-run path still uses.

use std::fmt::Write as _;
use std::io::{IsTerminal, Write};
use std::time::Duration;

/// Braille spinner frames; each is one display column wide.
const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// How a finished row reads — drives both its glyph and color.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// The step did real work (push/create/update): green `✓`.
    Done,
    /// The step was a no-op (skipped/up-to-date/merged/deleted):
    /// dim `·`.
    Noop,
}

#[derive(Clone, Copy)]
enum State {
    Pending,
    Active,
    Resolved(Mark),
}

struct Row {
    /// PR number, once known. A freshly-created PR has none until
    /// its POST returns — the row fills the number in on resolve.
    number: Option<u64>,
    /// Long, truncatable middle. Empty for non-PR steps whose text
    /// lives entirely in `word`.
    title: String,
    /// Trailing status word ("queued" / "updating" / "updated").
    word: String,
    url: Option<String>,
    state: State,
}

pub struct Progress {
    interactive: bool,
    color: bool,
    width: usize,
    frame: usize,
    rows: Vec<Row>,
    /// Physical lines written by the last redraw, so the next one
    /// knows how far up to move the cursor.
    drawn: usize,
    transcript: Vec<String>,
}

impl Progress {
    /// Detect the output mode from stdout. Interactive (cursor
    /// redraw + spinner + color) only on a real terminal whose
    /// width we can read and with `NO_COLOR` unset.
    #[must_use]
    pub fn new() -> Self {
        let stdout = std::io::stdout();
        let (interactive, width) = match terminal_size::terminal_size() {
            Some((terminal_size::Width(w), _)) if stdout.is_terminal() => (true, w as usize),
            _ => (false, 0),
        };
        let color = interactive && std::env::var_os("NO_COLOR").is_none();
        Self::with_mode(interactive, color, width)
    }

    fn with_mode(interactive: bool, color: bool, width: usize) -> Self {
        Self {
            interactive,
            color,
            width: width.max(20),
            frame: 0,
            rows: Vec::new(),
            drawn: 0,
            transcript: Vec::new(),
        }
    }

    /// Print a standalone line above or below the live block (the
    /// header, the rebase note, the final summary). Only safe
    /// before the first row is added or after [`finish`]; never
    /// interleave with a live block.
    pub fn note(&mut self, line: impl Into<String>) {
        let line = line.into();
        println!("{line}");
        self.transcript.push(line);
    }

    /// Add a step that will run: a PR row (`title` set) or a plain
    /// status step (`title` empty, text in `word`). Starts
    /// `Pending`; interactive mode draws it immediately so the user
    /// sees the whole plan up front.
    pub fn add(
        &mut self,
        number: Option<u64>,
        title: impl Into<String>,
        word: impl Into<String>,
    ) -> usize {
        self.rows.push(Row {
            number,
            title: title.into(),
            word: word.into(),
            url: None,
            state: State::Pending,
        });
        self.redraw();
        self.rows.len() - 1
    }

    /// Add an already-finished row (a skip/up-to-date/merged commit
    /// that needs no network call). Records the transcript and, in
    /// plain mode, prints it.
    pub fn add_resolved(
        &mut self,
        number: Option<u64>,
        title: impl Into<String>,
        mark: Mark,
        word: impl Into<String>,
        url: Option<String>,
    ) {
        let idx = self.add(number, title, word);
        self.rows[idx].url = url;
        self.resolve(idx, mark, None);
    }

    /// Mark a row active (spinner glyph) without awaiting anything —
    /// for a step whose work is synchronous and so can't be polled.
    pub fn activate(&mut self, idx: usize, word: impl Into<String>) {
        self.rows[idx].word = word.into();
        self.rows[idx].state = State::Active;
        self.redraw();
    }

    /// Drop the current rows and start a fresh block on the next
    /// draw. Interactive mode erases the live block first, so a
    /// transient phase (e.g. the pre-flight spinner) disappears
    /// before unrelated output — a child process's stdout, or the
    /// next block — takes its place.
    pub fn clear_block(&mut self) {
        if self.interactive && self.drawn > 0 {
            let mut out = std::io::stdout().lock();
            let _ = write!(out, "\r\x1b[{}A\x1b[0J", self.drawn);
            let _ = out.flush();
        }
        self.rows.clear();
        self.drawn = 0;
    }

    /// Mark a row active and spin it while `fut` runs, redrawing on
    /// a fixed cadence so the spinner animates even across a single
    /// slow request. In plain mode this just awaits `fut`.
    pub async fn run<F: std::future::Future>(
        &mut self,
        idx: usize,
        active_word: impl Into<String>,
        fut: F,
    ) -> F::Output {
        self.activate(idx, active_word);

        if !self.interactive {
            return fut.await;
        }
        let mut fut = std::pin::pin!(fut);
        loop {
            match tokio::time::timeout(Duration::from_millis(90), fut.as_mut()).await {
                Ok(out) => return out,
                Err(_elapsed) => {
                    self.frame = (self.frame + 1) % FRAMES.len();
                    self.redraw();
                }
            }
        }
    }

    /// Resolve a row to its terminal state. `word` replaces the
    /// trailing word when given (e.g. "updating" → "updated").
    pub fn resolve(&mut self, idx: usize, mark: Mark, word: Option<&str>) {
        let row = &mut self.rows[idx];
        if let Some(w) = word {
            row.word = w.to_string();
        }
        row.state = State::Resolved(mark);
        let line = render_row(row, self.frame, usize::MAX, false);
        self.transcript.push(line.clone());
        if self.interactive {
            self.redraw();
        } else {
            println!("{line}");
        }
    }

    /// Fill in the number/url of a row whose PR was just created.
    pub fn set_pr(&mut self, idx: usize, number: Option<u64>, url: Option<String>) {
        let row = &mut self.rows[idx];
        if number.is_some() {
            row.number = number;
        }
        if url.is_some() {
            row.url = url;
        }
    }

    /// Print the closing line and hand back the full transcript.
    #[must_use]
    pub fn finish(mut self, line: impl Into<String>) -> Vec<String> {
        self.note(line);
        self.transcript
    }

    fn redraw(&mut self) {
        if !self.interactive {
            return;
        }
        if let Some((terminal_size::Width(w), _)) = terminal_size::terminal_size() {
            self.width = (w as usize).max(20);
        }
        let mut buf = String::new();
        if self.drawn > 0 {
            let _ = write!(buf, "\x1b[{}A", self.drawn);
        }
        for row in &self.rows {
            buf.push_str("\r\x1b[2K");
            buf.push_str(&render_row(row, self.frame, self.width, self.color));
            buf.push('\n');
        }
        self.drawn = self.rows.len();
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(buf.as_bytes());
        let _ = out.flush();
    }
}

impl Default for Progress {
    fn default() -> Self {
        Self::new()
    }
}

const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const CYAN: &str = "\x1b[36m";
const RESET: &str = "\x1b[0m";

/// Render one row to a single line, truncating the title so the
/// visible text fits `width` (use `usize::MAX` for no limit, e.g.
/// the transcript). Color wraps only the fixed-width glyph and
/// trailing word, so the width budget is computed on plain text.
fn render_row(row: &Row, frame: usize, width: usize, color: bool) -> String {
    let (glyph, glyph_color) = match row.state {
        State::Pending => ("◦", DIM),
        State::Active => (FRAMES[frame % FRAMES.len()], CYAN),
        State::Resolved(Mark::Done) => ("✓", GREEN),
        State::Resolved(Mark::Noop) => ("·", DIM),
    };
    let number = row.number.map_or_else(String::new, |n| format!("#{n}"));

    // Fixed (non-title) segments, joined later by single spaces.
    let mut head = vec![glyph.to_string()];
    if !number.is_empty() {
        head.push(number);
    }
    let mut tail = vec![row.word.clone()];
    if let Some(url) = &row.url {
        tail.push(url.clone());
    }

    // Budget the title against everything else already on the line.
    // +2 leading indent, +1 space between every segment.
    let fixed: usize = head.iter().chain(&tail).map(|s| s.chars().count()).sum::<usize>()
        + head.len()
        + tail.len() // one space before each segment (incl. the title slot)
        + 2; // leading indent
    let title = if row.title.is_empty() {
        String::new()
    } else {
        truncate(&row.title, width.saturating_sub(fixed))
    };

    let glyph_seg = if color {
        format!("{glyph_color}{glyph}{RESET}")
    } else {
        head[0].clone()
    };
    let word = tail[0].clone();
    let word_seg = if color {
        format!("{DIM}{word}{RESET}")
    } else {
        word
    };

    let mut out = String::from("  ");
    out.push_str(&glyph_seg);
    if head.len() > 1 {
        out.push(' ');
        out.push_str(&head[1]);
    }
    if !title.is_empty() {
        out.push(' ');
        out.push_str(&title);
    }
    out.push(' ');
    out.push_str(&word_seg);
    if let Some(url) = &row.url {
        out.push(' ');
        out.push_str(url);
    }
    out
}

/// Truncate to at most `max` display columns, with a trailing `…`
/// when shortened. Returns empty if there is no room.
fn truncate(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    if max == 1 {
        return "…".to_string();
    }
    let kept: String = s.chars().take(max - 1).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(number: Option<u64>, title: &str, word: &str, state: State, url: Option<&str>) -> Row {
        Row {
            number,
            title: title.to_string(),
            word: word.to_string(),
            url: url.map(str::to_string),
            state,
        }
    }

    #[test]
    fn resolved_row_renders_glyph_number_title_word_url() {
        let r = row(
            Some(1618),
            "fix(stack): restore prefix-match",
            "updated",
            State::Resolved(Mark::Done),
            Some("https://example/pull/1618"),
        );
        let line = render_row(&r, 0, usize::MAX, false);
        assert_eq!(
            line,
            "  ✓ #1618 fix(stack): restore prefix-match updated https://example/pull/1618"
        );
    }

    #[test]
    fn pending_row_uses_hollow_glyph_and_no_url_when_absent() {
        let r = row(Some(7), "title", "queued", State::Pending, None);
        assert_eq!(render_row(&r, 0, usize::MAX, false), "  ◦ #7 title queued");
    }

    #[test]
    fn active_row_shows_current_spinner_frame() {
        let r = row(Some(7), "t", "updating", State::Active, None);
        let line = render_row(&r, 2, usize::MAX, false);
        assert!(
            line.starts_with(&format!("  {} ", FRAMES[2])),
            "got: {line}"
        );
    }

    #[test]
    fn create_row_without_number_omits_the_hash() {
        let r = row(None, "new thing", "creating", State::Active, None);
        let line = render_row(&r, 0, usize::MAX, false);
        assert!(!line.contains('#'), "got: {line}");
        assert!(line.contains("new thing"), "got: {line}");
    }

    #[test]
    fn title_is_truncated_to_fit_width_keeping_glyph_number_word() {
        let r = row(
            Some(42),
            "a very long pull request title that will not fit",
            "updated",
            State::Resolved(Mark::Done),
            None,
        );
        let line = render_row(&r, 0, 30, false);
        assert!(
            line.chars().count() <= 30,
            "len {}: {line}",
            line.chars().count()
        );
        assert!(line.contains('…'), "got: {line}");
        assert!(line.contains("#42"), "got: {line}");
        assert!(line.ends_with("updated"), "got: {line}");
    }

    #[test]
    fn color_wraps_glyph_and_word_but_not_title() {
        let r = row(
            Some(1),
            "plaintitle",
            "updated",
            State::Resolved(Mark::Done),
            None,
        );
        let line = render_row(&r, 0, usize::MAX, true);
        assert!(line.contains(GREEN), "glyph should be green: {line}");
        assert!(line.contains("plaintitle"), "title stays plain: {line}");
        assert!(
            line.contains(&format!("{DIM}updated{RESET}")),
            "word dim: {line}"
        );
    }

    #[test]
    fn noop_row_uses_dim_dot() {
        let r = row(Some(9), "t", "merged", State::Resolved(Mark::Noop), None);
        assert!(render_row(&r, 0, usize::MAX, false).starts_with("  · "));
    }

    #[test]
    fn plain_mode_records_transcript_on_resolve_only() {
        let mut p = Progress::with_mode(false, false, 80);
        let idx = p.add(Some(5), "title", "queued");
        assert!(p.transcript.is_empty(), "pending must not record");
        p.resolve(idx, Mark::Done, Some("updated"));
        assert_eq!(p.transcript, vec!["  ✓ #5 title updated".to_string()]);
    }

    #[test]
    fn clear_block_resets_rows_and_draw_state() {
        let mut p = Progress::with_mode(false, false, 80);
        let _ = p.add(None, "", "fetching");
        p.clear_block();
        assert!(p.rows.is_empty());
        assert_eq!(p.drawn, 0);
        // A fresh row starts at index 0 again.
        assert_eq!(p.add(Some(1), "t", "queued"), 0);
    }

    #[test]
    fn set_pr_fills_number_for_a_created_row() {
        let mut p = Progress::with_mode(false, false, 80);
        let idx = p.add(None, "new pr", "queued");
        p.set_pr(
            idx,
            Some(1630),
            Some("https://example/pull/1630".to_string()),
        );
        p.resolve(idx, Mark::Done, Some("created"));
        assert_eq!(
            p.transcript,
            vec!["  ✓ #1630 new pr created https://example/pull/1630".to_string()]
        );
    }
}
