//! Unicode box-drawing characters for indented tree output.
//!
//! Each tree row pairs a *branch* prefix (the connector for the
//! row itself) with a *continuation* prefix (the column drawn
//! beneath the row to keep the visual lineage clear). Whether a
//! row is the last child of its parent flips both:
//!
//! ```text
//! parent
//! ├── child A          ← BRANCH                ┐
//! │   └── grandchild   ← CONTINUATION + LAST_  │ child A is not last,
//! ├── child B                                  │ so its continuation
//! │   ├── grandchild                           │ column draws `│   `
//! │   └── grandchild                           ┘
//! └── child C          ← LAST_BRANCH           ┐
//!     └── grandchild   ← LAST_CONTINUATION+LAST│ last child: column
//!                                              │ collapses to spaces
//! ```
//!
//! Use [`branch_chars`] when you have a `(is_last, ...)` decision
//! and want both prefixes back in one call.

/// Branch connector for a non-last child: `├── `.
pub const BRANCH: &str = "├── ";

/// Branch connector for the last child of its parent: `└── `.
pub const LAST_BRANCH: &str = "└── ";

/// Continuation column under a non-last child (keeps the vertical
/// pipe drawn so descendants stay visually attached): `│   `.
pub const CONTINUATION: &str = "│   ";

/// Continuation column under the last child (no more vertical
/// pipe — the lineage stops here): `    ` (four spaces).
pub const LAST_CONTINUATION: &str = "    ";

/// Pick the `(branch, continuation)` pair for a row based on
/// whether it's the last child of its parent.
#[must_use]
pub fn branch_chars(is_last: bool) -> (&'static str, &'static str) {
    if is_last {
        (LAST_BRANCH, LAST_CONTINUATION)
    } else {
        (BRANCH, CONTINUATION)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_child_uses_corner_and_blank_continuation() {
        let (branch, cont) = branch_chars(true);
        assert_eq!(branch, "└── ");
        assert_eq!(cont, "    ");
    }

    #[test]
    fn middle_child_uses_tee_and_pipe_continuation() {
        let (branch, cont) = branch_chars(false);
        assert_eq!(branch, "├── ");
        assert_eq!(cont, "│   ");
    }
}
