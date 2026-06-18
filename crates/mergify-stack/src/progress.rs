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
//! The same plain-streaming fallback also kicks in mid-flight when
//! the live block would grow taller than the viewport (see
//! [`Progress::would_overflow`]): a block taller than the terminal
//! can't be redrawn with a single cursor-up without corrupting
//! scrollback, so we flush what's resolved as permanent lines and
//! stream the rest.
//!
//! The full rendered text is also collected into a transcript
//! (returned by [`Progress::finish`]) so callers can keep the
//! buffered-lines contract the dry-run path still uses.

use std::fmt::Write as _;
use std::io::{IsTerminal, Write};
use std::time::Duration;

use mergify_tui::Theme;
use unicode_width::UnicodeWidthChar;

/// Braille spinner frames; each is one display column wide.
const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Glyph advance cadence — fast enough to read as smooth motion.
const SPIN_INTERVAL: Duration = Duration::from_millis(80);

/// How a finished row reads — drives both its glyph and color.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// The step did real work (push/create/update): green `✓`.
    Done,
    /// The step was a no-op (skipped/up-to-date/deleted): dim `·`.
    Noop,
    /// The commit was already merged on the trunk: magenta `·`,
    /// matching GitHub's merged badge. A distinct variant rather
    /// than a string-compare on the status word so the producer's
    /// `SkipMerged` semantics drive the color directly.
    Merged,
}

/// Whether a row is a bare status step or a per-PR row. Explicit so
/// a legitimately empty PR title can't be mistaken for a step row
/// (which would mis-indent it and drop it from column alignment).
#[derive(Clone, Copy, PartialEq, Eq)]
enum RowKind {
    Step,
    Pr,
}

#[derive(Clone, Copy)]
enum State {
    Pending,
    Active,
    Resolved(Mark),
}

struct Row {
    kind: RowKind,
    /// Long, truncatable middle (PR rows only). Empty for step rows
    /// whose text lives entirely in `word`.
    title: String,
    /// Trailing status word ("queued" / "updating" / "updated").
    word: String,
    /// Optional dim detail between the status tag and the url — the
    /// short SHA transition (`old7→new7` for an update, `new7` for a
    /// create). `None` leaves the segment out entirely.
    detail: Option<String>,
    /// PR URL, once known (a freshly-created PR has none until its
    /// POST returns). It already carries the PR number, so the row
    /// doesn't repeat it.
    url: Option<String>,
    state: State,
}

pub struct Progress {
    interactive: bool,
    theme: Theme,
    width: usize,
    /// Terminal height in rows. A live block taller than this can't
    /// be redrawn with a single cursor-up, so we fall back to plain
    /// streaming before it overflows.
    height: usize,
    frame: usize,
    rows: Vec<Row>,
    /// Physical lines written by the last redraw, so the next one
    /// knows how far up to move the cursor.
    drawn: usize,
    /// Cached `(status_w, url_w, detail_w)` PR-column widths.
    /// Recomputed only when a row is added or its word/url/detail
    /// changes — not on every redraw frame. `detail_w` keeps the SHA
    /// detail a padded column so titles line up even when some rows
    /// carry a detail and others don't.
    cols: Option<(usize, usize, usize)>,
    transcript: Vec<String>,
}

impl Progress {
    /// Detect the output mode from stdout. Interactive (cursor
    /// redraw + spinner) only on a real terminal whose size we can
    /// read. Color is delegated to [`Theme::detect`] (TTY-and-
    /// `NO_COLOR`-aware, suppressed under tests).
    #[must_use]
    pub fn new() -> Self {
        let stdout = std::io::stdout();
        let (interactive, width, height) = match terminal_size::terminal_size() {
            Some((terminal_size::Width(w), terminal_size::Height(h))) if stdout.is_terminal() => {
                (true, w as usize, h as usize)
            }
            _ => (false, 0, 0),
        };
        // Color follows interactivity: only an interactive terminal
        // (one whose size we could read) gets ANSI. A TTY whose size
        // probe failed streams plain, so it must stay plain — don't
        // let `Theme::detect` (which keys only on is_terminal) leak
        // color into that fallback.
        let theme = if interactive {
            Theme::detect()
        } else {
            Theme::new(false)
        };
        Self::with_mode(interactive, theme, width, height)
    }

    fn with_mode(interactive: bool, theme: Theme, width: usize, height: usize) -> Self {
        Self {
            interactive,
            theme,
            width: width.max(20),
            height: height.max(1),
            frame: 0,
            rows: Vec::new(),
            drawn: 0,
            cols: None,
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

    /// Add a plain status step (text in `word`, no PR title). Starts
    /// `Pending`; interactive mode draws it immediately so the user
    /// sees the whole plan up front.
    pub fn add(&mut self, word: impl Into<String>) -> usize {
        self.push_row(RowKind::Step, String::new(), word.into(), None, None);
        self.rows.len() - 1
    }

    /// Add a PR row (truncatable `title`, optional dim SHA `detail`).
    /// Starts `Pending`.
    pub fn add_pr(
        &mut self,
        title: impl Into<String>,
        word: impl Into<String>,
        detail: Option<String>,
        url: Option<String>,
    ) -> usize {
        self.push_row(RowKind::Pr, title.into(), word.into(), detail, url);
        self.rows.len() - 1
    }

    fn push_row(
        &mut self,
        kind: RowKind,
        title: String,
        word: String,
        detail: Option<String>,
        url: Option<String>,
    ) {
        self.rows.push(Row {
            kind,
            title,
            word,
            detail,
            url,
            state: State::Pending,
        });
        self.recompute_cols();
        // Adding a row may make the live block taller than the
        // viewport; degrade to plain streaming before it overflows.
        if self.interactive && self.would_overflow() {
            self.degrade_to_streaming();
        }
        self.redraw();
    }

    /// Add an already-finished step row (a skip that needs no
    /// network call).
    pub fn add_resolved(&mut self, mark: Mark, word: impl Into<String>) {
        let idx = self.add(word);
        self.resolve(idx, mark, None);
    }

    /// Add an already-finished PR row (a skip/up-to-date/merged
    /// commit that needs no network call).
    pub fn add_resolved_pr(
        &mut self,
        title: impl Into<String>,
        mark: Mark,
        word: impl Into<String>,
        url: Option<String>,
    ) {
        let idx = self.add_pr(title, word, None, url);
        self.resolve(idx, mark, None);
    }

    /// Mark a row active (spinner glyph) and relabel it, without
    /// awaiting. The frame is advanced only by the tick loop in
    /// [`Progress::run`], so the glyph stays put here — use this for
    /// a synchronous step that can't be polled.
    pub fn activate(&mut self, idx: usize, word: impl Into<String>) {
        self.rows[idx].word = word.into();
        self.rows[idx].state = State::Active;
        self.recompute_cols();
        self.redraw();
    }

    /// Finalize the rows drawn so far as permanent scrollback and
    /// start a fresh block on the next draw. Call before emitting
    /// output the reporter doesn't control (a child process's
    /// stdout) so the next redraw won't move the cursor back up over
    /// it. Prints nothing — the resolved lines stay on screen, so
    /// the spinner never blinks out to a blank gap.
    pub fn seal(&mut self) {
        self.rows.clear();
        self.cols = None;
        self.drawn = 0;
    }

    /// Mark a row active and spin it smoothly while `fut` runs,
    /// advancing the glyph on a fixed cadence regardless of when the
    /// work makes progress. In plain mode this just awaits `fut`.
    pub async fn run<F: std::future::Future>(
        &mut self,
        idx: usize,
        active_word: impl Into<String>,
        fut: F,
    ) -> F::Output {
        self.run_reporting(idx, active_word, fut, || None).await
    }

    /// Run `fut` under row `opt_idx` when `Some`, else just await it.
    /// Collapses the repeated `if let Some(idx) = … { run } else {
    /// await }` branch at the call sites that only conditionally
    /// have a row to attach the work to.
    pub async fn run_optional<F: std::future::Future>(
        &mut self,
        opt_idx: Option<usize>,
        active_word: impl Into<String>,
        fut: F,
    ) -> F::Output {
        if let Some(idx) = opt_idx {
            self.run(idx, active_word, fut).await
        } else {
            fut.await
        }
    }

    /// Like [`Progress::run`], but re-reads `label` on every tick so
    /// a step can relabel itself mid-flight (e.g. the pull number the
    /// fetch loop is currently on) while the glyph keeps spinning
    /// smoothly. `label` returns `None` to leave the label untouched.
    pub async fn run_reporting<F, L>(
        &mut self,
        idx: usize,
        active_word: impl Into<String>,
        fut: F,
        mut label: L,
    ) -> F::Output
    where
        F: std::future::Future,
        L: FnMut() -> Option<String>,
    {
        self.activate(idx, active_word);

        if !self.interactive {
            return fut.await;
        }
        let mut fut = std::pin::pin!(fut);
        loop {
            match tokio::time::timeout(SPIN_INTERVAL, fut.as_mut()).await {
                Ok(out) => return out,
                Err(_elapsed) => {
                    if let Some(word) = label() {
                        self.rows[idx].word = word;
                        self.recompute_cols();
                    }
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
        self.recompute_cols();
        let line = render_row(
            &self.rows[idx],
            self.frame,
            usize::MAX,
            &Theme::new(false),
            None,
        );
        self.transcript.push(line.clone());
        if self.interactive {
            self.redraw();
        } else {
            println!("{line}");
        }
    }

    /// Fill in the url of a row whose PR was just created.
    pub fn set_url(&mut self, idx: usize, url: Option<String>) {
        if url.is_some() {
            self.rows[idx].url = url;
            self.recompute_cols();
        }
    }

    /// Print the closing line and hand back the full transcript.
    #[must_use]
    pub fn finish(mut self, line: impl Into<String>) -> Vec<String> {
        self.note(line);
        self.transcript
    }

    /// True when the current row count would not fit in the
    /// viewport, leaving a row for the cursor. A block this tall
    /// can't be redrawn with a single cursor-up.
    fn would_overflow(&self) -> bool {
        self.rows.len() > self.height.saturating_sub(1)
    }

    /// Permanently flush the live block and switch to plain
    /// streaming. Clears the current in-place block, reprints the
    /// already-resolved rows as permanent scrollback lines, and
    /// drops out of interactive mode so future resolves stream one
    /// plain line each (no cursor-up — corruption-proof). Pending /
    /// active rows are dropped from the block; they print when they
    /// later resolve.
    fn degrade_to_streaming(&mut self) {
        let mut buf = String::new();
        if self.drawn > 0 {
            // Move to the top of the block and erase it, so the
            // resolved rows reprint cleanly as permanent lines.
            let _ = write!(buf, "\x1b[{}A\r\x1b[0J", self.drawn);
        }
        for row in &self.rows {
            if matches!(row.state, State::Resolved(_)) {
                buf.push_str(&render_row(
                    row,
                    self.frame,
                    self.width,
                    &self.theme,
                    self.cols,
                ));
                buf.push('\n');
            }
        }
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(buf.as_bytes());
        let _ = out.flush();
        self.interactive = false;
        self.drawn = 0;
    }

    fn redraw(&mut self) {
        if !self.interactive {
            return;
        }
        if let Some((terminal_size::Width(w), terminal_size::Height(h))) =
            terminal_size::terminal_size()
        {
            self.width = (w as usize).max(20);
            self.height = (h as usize).max(1);
        }
        let cols = self.cols;
        let mut buf = String::new();
        if self.drawn > 0 {
            let _ = write!(buf, "\x1b[{}A", self.drawn);
        }
        for row in &self.rows {
            buf.push_str("\r\x1b[2K");
            buf.push_str(&render_row(row, self.frame, self.width, &self.theme, cols));
            buf.push('\n');
        }
        self.drawn = self.rows.len();
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(buf.as_bytes());
        let _ = out.flush();
    }

    /// Recompute and cache the widest `[status]` and url across the
    /// current PR rows, so the live redraw can pad them to aligned
    /// columns without re-measuring every frame. `None` when there
    /// are no PR rows (nothing to align).
    fn recompute_cols(&mut self) {
        let mut status_w = 0;
        let mut url_w = 0;
        let mut detail_w = 0;
        let mut any = false;
        for r in self.rows.iter().filter(|r| r.kind == RowKind::Pr) {
            any = true;
            status_w = status_w.max(r.word.chars().count() + 2); // + "[]"
            url_w = url_w.max(r.url.as_deref().map_or(0, |u| u.chars().count()));
            detail_w = detail_w.max(r.detail.as_deref().map_or(0, |d| d.chars().count()));
        }
        self.cols = any.then_some((status_w, url_w, detail_w));
    }
}

impl Default for Progress {
    fn default() -> Self {
        Self::new()
    }
}

/// Render one row to a single line.
///
/// A step row is just `glyph + label`. A PR row is `glyph [status]
/// detail  url  title`, where `detail` (the dim SHA transition) is a
/// reserved column — a row without one keeps a blank slot rather than
/// dropping the field, so the url and title hold a fixed position
/// instead of sliding left: a display column in the live view, a
/// stable `cut -f` field when piped (tabs can't align display columns
/// once details differ in width). With
/// `cols = Some((status_w, url_w, detail_w))` status, detail, and url
/// are space-padded to those widths so rows line up under each other
/// (the live view); the detail column collapses only when no row in
/// the block carries one. With `cols = None` the same four fields are
/// tab-separated instead — every PR row emits all four (empty
/// detail/url included) so the piped transcript stays `cut -f`-able,
/// where streaming can't align anyway. The whole
/// line is truncated (tab-aware, Unicode-width-aware) to `width` so
/// the cursor-up redraw never wraps; pass `usize::MAX` for the
/// un-truncated transcript. Only the glyph and the `[status]`/detail
/// segments are colored, so the width budget is plain text.
fn render_row(
    row: &Row,
    frame: usize,
    width: usize,
    theme: &Theme,
    cols: Option<(usize, usize, usize)>,
) -> String {
    let (glyph, glyph_style) = match row.state {
        State::Pending => ("◦", theme.dim),
        State::Active => (FRAMES[frame % FRAMES.len()], theme.cyan),
        State::Resolved(Mark::Done) => ("✓", theme.green),
        State::Resolved(Mark::Noop) => ("·", theme.dim),
        State::Resolved(Mark::Merged) => ("·", theme.magenta),
    };

    let status = format!("[{word}]", word = row.word);
    // PR rows nest one level under the step they belong to (the push)
    // — four spaces — while bare step rows stay at the top level with
    // two.
    let plain = match row.kind {
        RowKind::Step => format!("  {glyph} {word}", word = row.word),
        RowKind::Pr => {
            // The url already carries the PR number, so the title
            // field is just the title.
            let url = row.url.as_deref().unwrap_or("");
            let title = &row.title;
            // Aligned (live view): pad status, the SHA detail, and url
            // to fixed columns so titles line up even across rows where
            // some carry a detail and some don't. The detail column is
            // dropped entirely when no row has one.
            if let Some((status_w, url_w, detail_w)) = cols {
                if detail_w > 0 {
                    let detail = row.detail.as_deref().unwrap_or("");
                    format!(
                        "    {glyph} {status:<status_w$}  {detail:<detail_w$}  {url:<url_w$}  {title}"
                    )
                } else {
                    format!("    {glyph} {status:<status_w$}  {url:<url_w$}  {title}")
                }
            } else {
                // Tab-separated (piped transcript): always four fields —
                // status, the SHA detail (empty when this row has none),
                // url, title — so `cut -f` columns stay fixed and a
                // detail-less row (a skip or an orphan delete) doesn't
                // shift its url a field left of the create/update rows it
                // streams beside. Mirrors the aligned view's reserved
                // detail column.
                let detail = row.detail.as_deref().unwrap_or("");
                format!("    {glyph} {status}\t{detail}\t{url}\t{title}")
            }
        }
    };
    let plain = truncate_display(&plain, width);

    if !theme.enabled {
        return plain;
    }
    // Color the glyph everywhere, and on PR rows the `[status]` tag
    // (state color) and the SHA `detail` (dim). They sit ahead of the
    // truncatable title, so first-match replaces are safe.
    let mut out = plain.replacen(
        glyph,
        &format!("{glyph_style}{glyph}{reset}", reset = theme.reset),
        1,
    );
    if row.kind == RowKind::Pr {
        out = out.replacen(
            &status,
            &format!("{glyph_style}{status}{reset}", reset = theme.reset),
            1,
        );
        if let Some(detail) = row.detail.as_deref() {
            if !detail.is_empty() && out.contains(detail) {
                out = out.replacen(
                    detail,
                    &format!("{dim}{detail}{reset}", dim = theme.dim, reset = theme.reset),
                    1,
                );
            }
        }
    }
    out
}

/// Display width of `s` in terminal columns: each tab expands to the
/// next multiple of 8, and every other char counts at its Unicode
/// display width (CJK/emoji are 2, combining marks 0).
fn display_width(s: &str) -> usize {
    s.chars().fold(0, |col, ch| {
        if ch == '\t' {
            (col / 8 + 1) * 8
        } else {
            col + UnicodeWidthChar::width(ch).unwrap_or(0)
        }
    })
}

/// Truncate `s` to at most `max` display columns (tab-aware,
/// Unicode-width-aware), with a trailing `…` when shortened, so it
/// occupies exactly one terminal line.
fn truncate_display(s: &str, max: usize) -> String {
    if display_width(s) <= max {
        return s.to_string();
    }
    let budget = max.saturating_sub(1); // leave a column for the `…`
    let mut col = 0;
    let mut out = String::new();
    for ch in s.chars() {
        let next = if ch == '\t' {
            (col / 8 + 1) * 8
        } else {
            col + UnicodeWidthChar::width(ch).unwrap_or(0)
        };
        if next > budget {
            break;
        }
        out.push(ch);
        col = next;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr_row(title: &str, word: &str, state: State, url: Option<&str>) -> Row {
        Row {
            kind: RowKind::Pr,
            title: title.to_string(),
            word: word.to_string(),
            detail: None,
            url: url.map(str::to_string),
            state,
        }
    }

    fn step_row(word: &str, state: State) -> Row {
        Row {
            kind: RowKind::Step,
            title: String::new(),
            word: word.to_string(),
            detail: None,
            url: None,
            state,
        }
    }

    fn plain() -> Theme {
        Theme::new(false)
    }

    fn colored() -> Theme {
        Theme::new(true)
    }

    #[test]
    fn resolved_pr_row_reserves_a_blank_detail_field() {
        // Tab mode always emits status, detail, url, title; a row with
        // no SHA detail keeps a blank field so its url stays in the
        // same `cut -f` column as the create/update rows beside it.
        let r = pr_row(
            "fix(stack): restore prefix-match",
            "updated",
            State::Resolved(Mark::Done),
            Some("https://example/pull/1618"),
        );
        assert_eq!(
            render_row(&r, 0, usize::MAX, &plain(), None),
            "    ✓ [updated]\t\thttps://example/pull/1618\tfix(stack): restore prefix-match"
        );
    }

    #[test]
    fn step_row_without_title_is_glyph_and_label_only() {
        let r = step_row("fetching trunk", State::Active);
        assert_eq!(
            render_row(&r, 0, usize::MAX, &plain(), None),
            format!("  {} fetching trunk", FRAMES[0])
        );
    }

    #[test]
    fn pending_pr_row_has_empty_detail_and_url_fields() {
        let r = pr_row("title", "queued", State::Pending, None);
        assert_eq!(
            render_row(&r, 0, usize::MAX, &plain(), None),
            "    ◦ [queued]\t\t\ttitle"
        );
    }

    #[test]
    fn empty_title_pr_row_stays_a_pr_row() {
        // A legitimately empty PR title must not collapse into a
        // step row (which would mis-indent and drop alignment).
        let r = pr_row("", "queued", State::Pending, None);
        let line = render_row(&r, 0, usize::MAX, &plain(), None);
        assert!(line.starts_with("    ◦ [queued]"), "got: {line}");
    }

    #[test]
    fn active_row_shows_current_spinner_frame() {
        let r = pr_row("t", "updating", State::Active, None);
        assert!(
            render_row(&r, 2, usize::MAX, &plain(), None)
                .starts_with(&format!("    {} ", FRAMES[2]))
        );
    }

    #[test]
    fn create_row_without_url_renders_just_the_title() {
        let r = pr_row("new thing", "creating", State::Active, None);
        let line = render_row(&r, 0, usize::MAX, &plain(), None);
        assert!(!line.contains('#'), "got: {line}");
        assert!(line.ends_with("new thing"), "got: {line}");
    }

    #[test]
    fn detail_segment_renders_between_status_and_url() {
        let mut r = pr_row("t", "updated", State::Resolved(Mark::Done), Some("u"));
        r.detail = Some("old7→new7".to_string());
        let line = render_row(&r, 0, usize::MAX, &plain(), None);
        assert_eq!(line, "    ✓ [updated]\told7→new7\tu\tt");
    }

    #[test]
    fn tab_mode_keeps_url_in_a_fixed_field_with_or_without_detail() {
        // The reported regression: a created row carries a SHA detail,
        // the orphan-delete row beside it carries none. Tab mode
        // reserves the detail field on both, so the url stays the same
        // `cut -f` field (field 3) rather than sliding a field left on
        // the detail-less row.
        let created = {
            let mut r = pr_row("t", "created", State::Resolved(Mark::Done), Some("u1645"));
            r.detail = Some("a649c67".to_string());
            r
        };
        let deleted = pr_row("t", "deleted", State::Resolved(Mark::Noop), Some("u1644"));
        let fields = |r: &Row| {
            render_row(r, 0, usize::MAX, &plain(), None)
                .split('\t')
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        let fc = fields(&created);
        let fd = fields(&deleted);
        assert_eq!(fc.len(), 4, "created: {fc:?}");
        assert_eq!(fd.len(), 4, "deleted: {fd:?}");
        assert_eq!((fc[1].as_str(), fc[2].as_str()), ("a649c67", "u1645"));
        assert_eq!((fd[1].as_str(), fd[2].as_str()), ("", "u1644"));
    }

    #[test]
    fn detail_segment_is_dimmed_when_colored() {
        let mut r = pr_row("t", "created", State::Resolved(Mark::Done), None);
        r.detail = Some("new1234".to_string());
        let line = render_row(&r, 0, usize::MAX, &colored(), Some((9, 0, 7)));
        let theme = colored();
        assert!(
            line.contains(&format!("{}new1234{}", theme.dim, theme.reset)),
            "got: {line}"
        );
    }

    #[test]
    fn long_row_is_truncated_to_one_line() {
        let r = pr_row(
            "a very long pull request title that will not fit",
            "updated",
            State::Resolved(Mark::Done),
            Some("https://example/pull/42"),
        );
        let line = render_row(&r, 0, 40, &plain(), None);
        assert!(
            display_width(&line) <= 40,
            "width {}: {line}",
            display_width(&line)
        );
        assert!(line.contains('…'), "got: {line}");
    }

    #[test]
    fn color_wraps_glyph_and_status_tag_but_not_title() {
        let r = pr_row("plaintitle", "updated", State::Resolved(Mark::Done), None);
        let theme = colored();
        let line = render_row(&r, 0, usize::MAX, &theme, None);
        assert!(
            line.starts_with(&format!("    {}✓{}", theme.green, theme.reset)),
            "glyph green: {line}"
        );
        assert!(
            line.contains(&format!("{}[updated]{}", theme.green, theme.reset)),
            "status green: {line}"
        );
        assert!(line.contains("plaintitle"), "title plain: {line}");
    }

    #[test]
    fn noop_row_uses_dim_dot() {
        let r = pr_row("t", "skipped", State::Resolved(Mark::Noop), None);
        assert!(render_row(&r, 0, usize::MAX, &plain(), None).starts_with("    · "));
    }

    #[test]
    fn merged_mark_is_magenta_like_github() {
        let r = pr_row("t", "merged", State::Resolved(Mark::Merged), None);
        let theme = colored();
        let line = render_row(&r, 0, usize::MAX, &theme, None);
        // The merged glyph is the magenta dot.
        assert!(
            line.starts_with(&format!("    {}·{}", theme.magenta, theme.reset)),
            "got: {line}"
        );
        assert!(
            line.contains(&format!("{}[merged]{}", theme.magenta, theme.reset)),
            "got: {line}"
        );
    }

    #[test]
    fn aligned_cols_line_up_titles_across_rows() {
        // Different status widths and url lengths; the title column
        // must still start at the same display column in both rows.
        let a = pr_row(
            "title-a",
            "updated",
            State::Resolved(Mark::Done),
            Some("u1"),
        );
        let b = pr_row(
            "title-b",
            "up-to-date",
            State::Resolved(Mark::Noop),
            Some("longer-url"),
        );
        let cols = Some(("up-to-date".len() + 2, "longer-url".len(), 0));
        let col_of = |s: &str, needle: &str| s[..s.find(needle).unwrap()].chars().count();
        let la = render_row(&a, 0, usize::MAX, &plain(), cols);
        let lb = render_row(&b, 0, usize::MAX, &plain(), cols);
        assert_eq!(
            col_of(&la, "title-a"),
            col_of(&lb, "title-b"),
            "a: {la:?}\nb: {lb:?}"
        );
    }

    #[test]
    fn aligned_cols_line_up_titles_with_mixed_detail() {
        // One row carries a SHA detail, the other doesn't; the title
        // column must still start at the same display column — the
        // detail is a padded column, not an inline splice.
        let with = {
            let mut r = pr_row(
                "title-a",
                "updated",
                State::Resolved(Mark::Done),
                Some("u1"),
            );
            r.detail = Some("old7→new7".to_string());
            r
        };
        let without = pr_row(
            "title-b",
            "up-to-date",
            State::Resolved(Mark::Noop),
            Some("u2"),
        );
        let cols = Some(("up-to-date".len() + 2, 2, "old7→new7".chars().count()));
        let col_of = |s: &str, needle: &str| s[..s.find(needle).unwrap()].chars().count();
        let la = render_row(&with, 0, usize::MAX, &plain(), cols);
        let lb = render_row(&without, 0, usize::MAX, &plain(), cols);
        assert_eq!(
            col_of(&la, "title-a"),
            col_of(&lb, "title-b"),
            "a: {la:?}\nb: {lb:?}"
        );
    }

    #[test]
    fn display_width_expands_tabs_to_eight_column_stops() {
        assert_eq!(display_width("ab\tc"), 9);
        assert_eq!(display_width("\t"), 8);
        assert_eq!(display_width("plain"), 5);
    }

    #[test]
    fn display_width_counts_wide_glyphs_as_two_columns() {
        // CJK ideographs are double-width; naive char-count would
        // under-measure and let a too-wide line slip past truncation.
        let cjk = "日本語"; // 3 chars, 6 columns
        assert_eq!(cjk.chars().count(), 3);
        assert_eq!(display_width(cjk), 6);
        assert!(display_width(cjk) > cjk.chars().count());
    }

    #[test]
    fn truncate_respects_wide_glyph_budget() {
        let cjk = "日本語日本語日本語"; // 9 chars, 18 columns
        let out = truncate_display(cjk, 8);
        assert!(
            display_width(&out) <= 8,
            "width {}: {out}",
            display_width(&out)
        );
    }

    #[test]
    fn plain_mode_records_transcript_on_resolve_only() {
        let mut p = Progress::with_mode(false, plain(), 80, 24);
        let idx = p.add_pr("title", "queued", None, None);
        assert!(p.transcript.is_empty(), "pending must not record");
        p.resolve(idx, Mark::Done, Some("updated"));
        assert_eq!(p.transcript, vec!["    ✓ [updated]\t\t\ttitle".to_string()]);
    }

    #[test]
    fn seal_resets_rows_and_draw_state() {
        let mut p = Progress::with_mode(false, plain(), 80, 24);
        let _ = p.add("fetching");
        p.seal();
        assert!(p.rows.is_empty());
        assert_eq!(p.drawn, 0);
        assert!(p.cols.is_none());
        // A fresh row starts at index 0 again.
        assert_eq!(p.add_pr("t", "queued", None, None), 0);
    }

    #[test]
    fn set_url_fills_a_created_rows_url() {
        let mut p = Progress::with_mode(false, plain(), 80, 24);
        let idx = p.add_pr("new pr", "queued", None, None);
        p.set_url(idx, Some("https://example/pull/1630".to_string()));
        p.resolve(idx, Mark::Done, Some("created"));
        assert_eq!(
            p.transcript,
            vec!["    ✓ [created]\t\thttps://example/pull/1630\tnew pr".to_string()]
        );
    }

    #[test]
    fn cols_cache_tracks_widest_row() {
        let mut p = Progress::with_mode(false, plain(), 80, 24);
        p.add_pr("a", "queued", None, Some("short".into()));
        assert_eq!(p.cols, Some(("queued".len() + 2, "short".len(), 0)));
        p.add_pr("b", "up-to-date", None, Some("longer-url".into()));
        assert_eq!(
            p.cols,
            Some(("up-to-date".len() + 2, "longer-url".len(), 0))
        );
    }

    #[test]
    fn overflow_degrades_without_oversized_cursor_up() {
        // A row count that exceeds a tiny forced height must trip the
        // streaming fallback rather than emit a cursor-up larger than
        // the viewport (which would corrupt scrollback).
        let mut p = Progress::with_mode(true, plain(), 80, 3);
        // height 3 ⇒ block may hold at most 2 rows before overflow.
        p.add("a");
        p.add("b");
        assert!(p.interactive, "still fits");
        p.add("c"); // would need 3 live rows in a 3-row terminal
        assert!(!p.interactive, "must have degraded to streaming");
        // drawn is reset on degrade, so no later cursor-up can exceed
        // the viewport.
        assert_eq!(p.drawn, 0);
        assert!(
            p.drawn < p.height,
            "cursor-up ({}) must stay within height ({})",
            p.drawn,
            p.height
        );
    }
}
