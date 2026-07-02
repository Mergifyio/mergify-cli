//! Interactive fzf-style fuzzy picker.
//!
//! Thin boundary around `dialoguer::FuzzySelect` (whose matcher is
//! `fuzzy-matcher`'s `SkimMatcherV2` — skim is the Rust fzf clone)
//! so command crates never depend on dialoguer directly: the crate
//! stays swappable, and every future picker (`stack checkout`,
//! `stack edit`, …) shares one entry point with a single theming
//! and cancellation policy.

use std::io;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

use console::Term;
use dialoguer::FuzzySelect;
use dialoguer::theme::{ColorfulTheme, SimpleTheme, Theme};

use crate::theme::colors_enabled;

/// Set while a picker's `interact_opt()` call is on the stack, so the
/// SIGINT handler knows whether the terminal is currently in the
/// state dialoguer left it in (cursor hidden, raw mode) and needs
/// restoring.
static PICKER_ACTIVE: AtomicBool = AtomicBool::new(false);
static INSTALL_SIGINT: Once = Once::new();

/// Ctrl-C inside the picker must restore the terminal before the
/// process dies: console's key reader re-raises SIGINT after
/// restoring termios, but nothing un-hides the cursor dialoguer
/// hid. The handler exits 130 (128+SIGINT), the shell convention
/// fzf itself follows.
fn install_sigint_handler() {
    INSTALL_SIGINT.call_once(|| {
        // If installation fails (e.g. some embedder already owns
        // the handler), we keep console's raise semantics: death
        // by SIGINT, possibly with a hidden cursor — no worse
        // than before this handler existed.
        let _ = ctrlc::set_handler(|| {
            if PICKER_ACTIVE.load(Ordering::SeqCst) {
                let _ = Term::stderr().show_cursor();
            }
            std::process::exit(130);
        });
    });
}

/// Run an fzf-style fuzzy picker over `items` on the controlling
/// terminal: type to filter, arrows to move, Enter to accept.
/// `default` is the index preselected before any filtering.
///
/// Labels should be unique — dialoguer maps the selected label back
/// to an index by string equality, so duplicate labels resolve to
/// the first occurrence.
///
/// Ctrl-C normally exits the process directly with status 130 (the
/// fzf convention) via a process-global SIGINT handler installed on
/// first use, which also restores the cursor dialoguer hides during
/// interaction. `Ok(None)` cancellation from an
/// [`io::ErrorKind::Interrupted`] read error is defensive fallback
/// for when that handler is inert — SIGINT ignored, or installation
/// lost a race with another handler owner (e.g. under `nohup`).
///
/// Callers must only invoke this when stdin, stdout, and stderr are
/// all TTYs — dialoguer renders the list on stderr; there is no
/// non-interactive fallback at this layer.
pub fn fuzzy_select(prompt: &str, items: &[String], default: usize) -> io::Result<Option<usize>> {
    install_sigint_handler();
    let colorful = ColorfulTheme::default();
    let simple = SimpleTheme;
    // Same color policy as every other renderer (`--color` override
    // > `NO_COLOR` > `FORCE_COLOR`/`CLICOLOR_FORCE` > TTY), reused
    // from `theme.rs` rather than re-derived.
    let theme: &dyn Theme = if colors_enabled() { &colorful } else { &simple };
    PICKER_ACTIVE.store(true, Ordering::SeqCst);
    let result = FuzzySelect::with_theme(theme)
        .with_prompt(prompt)
        .items(items)
        .default(default)
        .highlight_matches(true)
        // The caller prints its own confirmation line; suppress
        // dialoguer's post-selection echo so nothing shows twice.
        .report(false)
        .interact_opt();
    if result.is_err() {
        // dialoguer restores the cursor on its Enter/Esc paths but
        // not when the key read errors. Restore BEFORE clearing
        // PICKER_ACTIVE: a SIGINT handler that observes `false` is
        // then guaranteed the restore already happened, and one
        // that observes `true` restores it itself — a double
        // restore is harmless, a missed one leaves the shell
        // cursorless.
        let _ = Term::stderr().show_cursor();
    }
    PICKER_ACTIVE.store(false, Ordering::SeqCst);
    map_interact_result(result)
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
    fn interrupted_read_is_cancel_fallback() {
        // Normally Ctrl-C exits the process directly via the SIGINT
        // handler (see `install_sigint_handler`) and this mapping
        // never runs; it only fires when that handler is inert.
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
