//! Interactive fzf-style fuzzy picker.
//!
//! Thin boundary around `dialoguer::FuzzySelect` (whose matcher is
//! `fuzzy-matcher`'s `SkimMatcherV2` — skim is the Rust fzf clone)
//! so command crates never depend on dialoguer directly: the crate
//! stays swappable, and every future picker (`stack checkout`,
//! `stack edit`, …) shares one entry point with a single theming
//! and cancellation policy.

use std::io;

use dialoguer::FuzzySelect;
use dialoguer::theme::{ColorfulTheme, SimpleTheme, Theme};

use crate::theme::colors_enabled;

/// Run an fzf-style fuzzy picker over `items` on the controlling
/// terminal: type to filter, arrows to move, Enter to accept.
/// `default` is the index preselected before any filtering.
///
/// Returns `Ok(None)` when the user cancels — Escape natively, or
/// Ctrl-C, which the raw-mode key reader surfaces as an
/// [`io::ErrorKind::Interrupted`] read error.
///
/// Callers must only invoke this when stdin and stdout are TTYs;
/// there is no non-interactive fallback at this layer.
pub fn fuzzy_select(prompt: &str, items: &[String], default: usize) -> io::Result<Option<usize>> {
    let colorful = ColorfulTheme::default();
    let simple = SimpleTheme;
    // Same color policy as every other renderer (`--color` override
    // > `NO_COLOR` > `FORCE_COLOR`/`CLICOLOR_FORCE` > TTY), reused
    // from `theme.rs` rather than re-derived.
    let theme: &dyn Theme = if colors_enabled() { &colorful } else { &simple };
    let result = FuzzySelect::with_theme(theme)
        .with_prompt(prompt)
        .items(items)
        .default(default)
        .highlight_matches(true)
        .max_length(picker_rows())
        // The caller prints its own confirmation line; suppress
        // dialoguer's post-selection echo so nothing shows twice.
        .report(false)
        .interact_opt();
    map_interact_result(result)
}

/// Visible rows for the list: terminal height minus the prompt
/// line, floored so a tiny terminal still shows a usable window.
fn picker_rows() -> usize {
    let height =
        terminal_size::terminal_size().map_or(24, |(_, terminal_size::Height(h))| usize::from(h));
    height.saturating_sub(2).max(3)
}

/// Map dialoguer's interact result onto the wrapper's contract:
/// cancellation becomes `Ok(None)`, everything else passes through.
fn map_interact_result(
    result: Result<Option<usize>, dialoguer::Error>,
) -> io::Result<Option<usize>> {
    match result {
        Ok(selection) => Ok(selection),
        Err(dialoguer::Error::IO(err)) => {
            if err.kind() == io::ErrorKind::Interrupted {
                Ok(None)
            } else {
                Err(err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_and_native_cancel_pass_through() {
        assert_eq!(map_interact_result(Ok(Some(2))).unwrap(), Some(2));
        assert_eq!(map_interact_result(Ok(None)).unwrap(), None);
    }

    #[test]
    fn ctrl_c_interrupted_read_is_cancel() {
        let interrupted = dialoguer::Error::IO(io::Error::new(
            io::ErrorKind::Interrupted,
            "read interrupted",
        ));
        assert_eq!(map_interact_result(Err(interrupted)).unwrap(), None);
    }

    #[test]
    fn other_io_errors_propagate() {
        let broken = dialoguer::Error::IO(io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe"));
        let err = map_interact_result(Err(broken)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }
}
