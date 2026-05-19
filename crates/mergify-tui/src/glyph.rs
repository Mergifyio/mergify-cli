//! Pairing of a Unicode icon with its [`anstyle::Style`].
//!
//! Multiple commands map an enum-ish state code (CI check states,
//! merge-queue batch states, …) to "render this icon in that color".
//! Both halves of that pair travel together at every call site: the
//! icon goes in the formatted output, the style wraps it. Bundling
//! them into one named type beats returning a `(&str, Style)` tuple
//! — the field names document what's what at the call site, and
//! future fields (e.g. a dim suffix) can be added without rewriting
//! every consumer.

use anstyle::Style;

#[derive(Clone, Copy)]
pub struct StyledGlyph {
    pub icon: &'static str,
    pub style: Style,
}

impl StyledGlyph {
    /// Convenience constructor — saves a few `StyledGlyph { … }`
    /// braces at the dense pattern-match call sites.
    #[must_use]
    pub const fn new(icon: &'static str, style: Style) -> Self {
        Self { icon, style }
    }
}
