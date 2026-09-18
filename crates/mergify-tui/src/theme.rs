//! ANSI styling, enabled by the recorded [`ColorChoice`] and the TTY.
//!
//! This crate reads no environment variable of its own: the
//! `NO_COLOR` family arrives folded into that choice. See
//! [`set_color_choice`].
//!
//! The intent is to write normal `format!` / `write!` code paths
//! that emit styled output on an interactive terminal and produce
//! plain text everywhere else, *without* conditional branching at
//! every call site. `anstyle::Style::new()` (the default)
//! deliberately emits no escape sequences in its `Display` impl —
//! so when [`Theme::detect`] decides colors are off, every named
//! style on the [`Theme`] is a `Style::new()` no-op and `reset`
//! is the empty string. Code reads the same in both modes.

use std::io::IsTerminal;
use std::sync::OnceLock;

use anstyle::AnsiColor;
use anstyle::Style;

/// The user's resolved color preference. `Auto` defers to TTY
/// detection; `Always`/`Never` override it. The `NO_COLOR` family is
/// folded in by the caller of [`set_color_choice`], not here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

static COLOR_CHOICE: OnceLock<ColorChoice> = OnceLock::new();

/// Record the process-wide color preference, once, at startup before
/// any [`Theme::detect`]. Subsequent calls are ignored, so a stray
/// second call can't flip colors mid-run.
///
/// Calling this is also what makes color possible at all: until it
/// does, [`Theme::detect`] reports disabled. Only the CLI entry point
/// calls it, so a process that never went through `main` — a test
/// harness, a doctest, an embedder — is never colored.
///
/// The caller passes the *resolved* choice: `--color` with the
/// `NO_COLOR` / `FORCE_COLOR` / `CLICOLOR_FORCE` overrides already
/// folded in (`mergify-cli`'s `resolve_color_choice`). This crate
/// deliberately never reads the environment itself. Two reasons:
/// it keeps `mergify-tui` dependency-light and free of the
/// workspace's env funnel, and it means no consumer crate's test
/// binary reads the environment on a worker thread just by
/// rendering a themed line.
pub fn set_color_choice(choice: ColorChoice) {
    let _ = COLOR_CHOICE.set(choice);
}

/// Pre-built styles + reset escape, matched to the renderers in
/// the ported commands. Each field is either a real `Style` (when
/// colors are enabled) or `Style::new()` (when disabled — emits
/// nothing); `reset` mirrors that with `"\x1b[0m"` vs `""`.
///
/// Construct via [`Theme::detect`] for the production policy (the
/// recorded choice, else the TTY; off outside the CLI entry point).
/// Tests that need to assert on styled output explicitly can pass
/// `enabled = true` to [`Theme::new`].
pub struct Theme {
    pub enabled: bool,
    pub bold: Style,
    pub dim: Style,
    /// SGR reset escape, or empty when colors are disabled. Using
    /// a `&'static str` instead of `anstyle::Reset` keeps both
    /// styled and plain code paths free of escape sequences when
    /// `enabled = false`.
    pub reset: &'static str,
    pub cyan: Style,
    pub green: Style,
    pub red: Style,
    pub yellow: Style,
    pub magenta: Style,
    /// Bold + yellow. Distinct named style because it shows up in
    /// every "warning"-flavored line (e.g. the queue pause
    /// indicator) and nesting `{B}{Y}` at every call site is
    /// noisy.
    pub warn: Style,
}

impl Theme {
    /// Detect whether the process should emit colors.
    ///
    /// Policy:
    ///
    /// 1. No [`set_color_choice`] yet ⇒ disabled. Only the CLI entry
    ///    point records one, so this is a test harness or an embedder:
    ///    it asserts on in-memory buffers and must not take a
    ///    dependency on the developer's terminal.
    /// 2. `--color always`/`never` (via [`set_color_choice`]) wins.
    /// 3. Otherwise (`auto`): `stdout` must be a terminal.
    ///
    /// `NO_COLOR` / `FORCE_COLOR` / `CLICOLOR_FORCE` are **not** read
    /// here — see [`set_color_choice`].
    #[must_use]
    pub fn detect() -> Self {
        Self::new(colors_enabled())
    }

    /// Construct with explicit `enabled`. Tests use this to
    /// deterministically exercise the styled or plain branch.
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        let on = |style: Style| if enabled { style } else { Style::new() };
        Self {
            enabled,
            bold: on(Style::new().bold()),
            dim: on(Style::new().dimmed()),
            reset: if enabled { "\x1b[0m" } else { "" },
            cyan: on(Style::new().fg_color(Some(AnsiColor::Cyan.into()))),
            green: on(Style::new().fg_color(Some(AnsiColor::Green.into()))),
            red: on(Style::new().fg_color(Some(AnsiColor::Red.into()))),
            yellow: on(Style::new().fg_color(Some(AnsiColor::Yellow.into()))),
            magenta: on(Style::new().fg_color(Some(AnsiColor::Magenta.into()))),
            warn: on(Style::new().bold().fg_color(Some(AnsiColor::Yellow.into()))),
        }
    }

    /// Build an arbitrary foreground color [`Style`] honoring the
    /// theme's enabled flag. Useful when a renderer maps domain
    /// state (status code, severity, …) to a color and the named
    /// fields above don't cover it.
    #[must_use]
    pub fn fg(&self, color: AnsiColor) -> Style {
        if self.enabled {
            Style::new().fg_color(Some(color.into()))
        } else {
            Style::new()
        }
    }
}

/// Pure color decision, factored out of [`colors_enabled`] so the
/// precedence is unit-testable without touching global state or the
/// real TTY.
fn resolve_enabled(choice: Option<ColorChoice>, is_tty: bool) -> bool {
    match choice {
        Some(ColorChoice::Always) => true,
        // `None` is nobody having recorded a preference, which means
        // nobody is watching a terminal — see [`set_color_choice`].
        None | Some(ColorChoice::Never) => false,
        Some(ColorChoice::Auto) => is_tty,
    }
}

pub(crate) fn colors_enabled() -> bool {
    // An unset choice means we are not the CLI: colors off. This
    // used to be `cfg!(test)`, which cannot do the job — it is false
    // whenever this crate is compiled as a dependency, so every
    // *consumer* crate's tests were reading the developer's
    // environment after all. `FORCE_COLOR=1 cargo test` failed on the
    // escape sequences that leaked into asserted output.
    resolve_enabled(COLOR_CHOICE.get().copied(), std::io::stdout().is_terminal())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_precedence() {
        // No recorded choice: not the CLI, so nothing is colored —
        // not even on a TTY. This is what keeps a consumer crate's
        // tests reproducible.
        assert!(!resolve_enabled(None, true));
        // Explicit choice overrides the TTY in both directions.
        assert!(resolve_enabled(Some(ColorChoice::Always), false));
        assert!(!resolve_enabled(Some(ColorChoice::Never), true));
        // Auto follows the TTY. The env overrides that used to be
        // decided here now reach us already folded into the choice —
        // see `set_color_choice`.
        assert!(resolve_enabled(Some(ColorChoice::Auto), true));
        assert!(!resolve_enabled(Some(ColorChoice::Auto), false));
    }

    #[test]
    fn disabled_theme_emits_no_escape_sequences() {
        let theme = Theme::new(false);
        assert_eq!(theme.reset, "");
        assert_eq!(format!("{}text{:#}", theme.bold, theme.bold), "text");
        assert_eq!(format!("{}text{:#}", theme.cyan, theme.cyan), "text");
        assert_eq!(
            format!(
                "{}text{:#}",
                theme.fg(AnsiColor::Blue),
                theme.fg(AnsiColor::Blue)
            ),
            "text",
        );
    }

    #[test]
    fn enabled_theme_wraps_with_codes() {
        let theme = Theme::new(true);
        assert_eq!(theme.reset, "\x1b[0m");
        // anstyle's `{:#}` prints the reset; we just need codes
        // surrounding the payload.
        let rendered = format!("{}text{}", theme.bold, theme.reset);
        assert!(rendered.starts_with("\x1b["), "got {rendered:?}");
        assert!(rendered.contains("text"));
        assert!(rendered.ends_with("\x1b[0m"));
    }

    #[test]
    fn fg_respects_enabled_flag() {
        assert_eq!(format!("{}", Theme::new(false).fg(AnsiColor::Red)), "");
        assert!(!format!("{}", Theme::new(true).fg(AnsiColor::Red)).is_empty());
    }
}
