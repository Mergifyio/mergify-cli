//! Terminal-UI primitives shared across the ported `mergify`
//! commands.
//!
//! Each command renders its own bespoke layout, but the building
//! blocks — color/TTY detection, relative-time formatter, tree
//! characters — are uniform. Centralizing them here keeps the
//! visual style consistent across `queue status`, `queue show`,
//! `freeze list`, and any future command that needs structured
//! human-readable output.
//!
//! Modules:
//!
//! - [`theme`]: [`Theme`] struct that wraps `anstyle::Style` with
//!   TTY-and-`NO_COLOR`-aware enable/disable, plus a named-color
//!   palette. The same closure-based emit code paths produce
//!   styled output on a TTY and plain text everywhere else with no
//!   conditional branching at every write.
//! - [`glyph`]: [`StyledGlyph`] — pairs a Unicode icon with the
//!   [`anstyle::Style`] it's drawn in. Used by commands that map
//!   state codes (check states, batch states, …) to a small visual
//!   token.
//! - [`time`]: [`relative_time`](time::relative_time) formats an
//!   ISO-8601/RFC-3339 timestamp as a coarse delta (`Ns` / `Nm` /
//!   `Nh` / `Nd`), with `~…` / `… ago` decorators for
//!   future/past. Returns an empty string on parse failure rather
//!   than panicking — degrading gracefully matches the Python
//!   originals' behavior.
//! - [`tree`]: Unicode box-drawing constants
//!   ([`BRANCH`](tree::BRANCH), [`LAST_BRANCH`](tree::LAST_BRANCH),
//!   [`CONTINUATION`](tree::CONTINUATION),
//!   [`LAST_CONTINUATION`](tree::LAST_CONTINUATION)) and the
//!   [`branch_chars`](tree::branch_chars) helper.
//! - [`select`]: [`fuzzy_select`](select::fuzzy_select) — fzf-style
//!   fuzzy picker over a list of labels, wrapping
//!   `dialoguer::FuzzySelect` behind this crate's boundary.

pub mod glyph;
pub mod select;
pub mod theme;
pub mod time;
pub mod tree;

pub use glyph::StyledGlyph;
pub use select::fuzzy_select;
pub use theme::{ColorChoice, Theme, set_color_choice};
pub use time::relative_time;
