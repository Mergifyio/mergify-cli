//! ANSI styling wrapped with TTY/`NO_COLOR` detection.
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

/// The user's `--color` preference. `Auto` defers to env vars and TTY
/// detection; `Always`/`Never` override both.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

static COLOR_CHOICE: OnceLock<ColorChoice> = OnceLock::new();

/// Record the process-wide color preference (from `--color`), once,
/// at startup before any [`Theme::detect`]. Subsequent calls are
/// ignored, so a stray second call can't flip colors mid-run.
pub fn set_color_choice(choice: ColorChoice) {
    let _ = COLOR_CHOICE.set(choice);
}

/// Pre-built styles + reset escape, matched to the renderers in
/// the ported commands. Each field is either a real `Style` (when
/// colors are enabled) or `Style::new()` (when disabled — emits
/// nothing); `reset` mirrors that with `"\x1b[0m"` vs `""`.
///
/// Construct via [`Theme::detect`] for the production policy
/// (TTY-only, `NO_COLOR`-aware, suppressed under `cfg!(test)`).
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
    /// 1. `cfg!(test)` ⇒ disabled. `cargo test` may inherit a TTY
    ///    parent stdout, but tests assert on in-memory buffers and
    ///    shouldn't take a dependency on the developer's terminal.
    /// 2. `--color always`/`never` (via [`set_color_choice`]) wins.
    /// 3. Otherwise (`auto`): `NO_COLOR` forces off; `FORCE_COLOR` /
    ///    `CLICOLOR_FORCE` force on (e.g. through a pager or CI
    ///    viewer); else `stdout` must be a terminal.
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
/// precedence is unit-testable without touching global state, env, or
/// the real TTY.
fn resolve_enabled(choice: ColorChoice, no_color: bool, force_color: bool, is_tty: bool) -> bool {
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => {
            if no_color {
                false
            } else if force_color {
                true
            } else {
                is_tty
            }
        }
    }
}

pub(crate) fn colors_enabled() -> bool {
    // Tests assert on in-memory buffers; never depend on the dev's TTY.
    if cfg!(test) {
        return false;
    }
    let choice = COLOR_CHOICE.get().copied().unwrap_or_default();
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let force_color =
        std::env::var_os("FORCE_COLOR").is_some() || std::env::var_os("CLICOLOR_FORCE").is_some();
    resolve_enabled(
        choice,
        no_color,
        force_color,
        std::io::stdout().is_terminal(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_precedence() {
        // Explicit choice overrides everything.
        assert!(resolve_enabled(ColorChoice::Always, true, false, false));
        assert!(!resolve_enabled(ColorChoice::Never, false, true, true));
        // Auto: NO_COLOR wins over FORCE_COLOR and TTY.
        assert!(!resolve_enabled(ColorChoice::Auto, true, true, true));
        // Auto: FORCE_COLOR turns it on without a TTY.
        assert!(resolve_enabled(ColorChoice::Auto, false, true, false));
        // Auto: otherwise follow the TTY.
        assert!(resolve_enabled(ColorChoice::Auto, false, false, true));
        assert!(!resolve_enabled(ColorChoice::Auto, false, false, false));
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
