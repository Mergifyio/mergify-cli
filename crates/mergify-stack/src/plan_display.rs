//! Shared renderer for the full-stack action-plan previews printed
//! by `drop`/`squash`/`fixup`/`reword`/`reorder`/`move`.
//!
//! Port of `mergify_cli/stack/reorder.py::{display_plan,
//! display_action_plan}`. Every plan lists the ENTIRE stack in
//! `base..HEAD` order, numbered 1..N, each row showing the 12-char
//! SHA, the subject, the 12-char Change-Id (when present), and an
//! optional ` [<action>]` tag. The render is plain text — no color,
//! no Rich markup — matching the CLI's deliberate plain output.

/// One row of a plan preview: a single stack commit plus the
/// action (if any) that this command applies to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRow {
    /// Full 40-hex commit SHA. Truncated to 12 chars when rendered.
    pub sha: String,
    /// Commit subject line.
    pub subject: String,
    /// The commit's `Change-Id:` trailer value, or empty string
    /// when absent. Rendered as ` (<change_id[:12]>)` when non-empty.
    pub change_id: String,
    /// The action tag for this row (e.g. `drop`, `fixup`, `reword`,
    /// `amend`), or `None` for untouched rows / tag-less plans.
    pub action: Option<String>,
}

/// Render a plan preview: the `title` line followed by one
/// formatted line per row, mirroring Python's
/// `display_plan` / `display_action_plan`.
///
/// Each row is `  {idx}. {sha[:12]} {subject}{cid}{tag}` where
/// `idx` is 1-based, `cid` is ` ({change_id[:12]})` when the
/// Change-Id is non-empty, and `tag` is ` [{action}]` when an
/// action is set.
#[must_use]
pub fn render_plan(title: &str, rows: &[PlanRow]) -> Vec<String> {
    let mut out = Vec::with_capacity(rows.len() + 1);
    out.push(title.to_string());
    for (idx, row) in rows.iter().enumerate() {
        let sha = truncate(&row.sha, 12);
        let cid = if row.change_id.is_empty() {
            String::new()
        } else {
            format!(" ({})", truncate(&row.change_id, 12))
        };
        let tag = match &row.action {
            Some(action) => format!(" [{action}]"),
            None => String::new(),
        };
        out.push(format!(
            "  {n}. {sha} {subject}{cid}{tag}",
            n = idx + 1,
            subject = row.subject,
        ));
    }
    out
}

fn truncate(s: &str, max: usize) -> &str {
    &s[..s.len().min(max)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(sha: &str, subject: &str, change_id: &str, action: Option<&str>) -> PlanRow {
        PlanRow {
            sha: sha.to_string(),
            subject: subject.to_string(),
            change_id: change_id.to_string(),
            action: action.map(str::to_string),
        }
    }

    #[test]
    fn title_only_when_no_rows() {
        assert_eq!(render_plan("Drop plan:", &[]), vec!["Drop plan:"]);
    }

    #[test]
    fn cid_shown_when_present() {
        let rows = [row(
            "abc123def4567890",
            "Add the thing",
            "I0123456789abcdef",
            None,
        )];
        assert_eq!(
            render_plan("Move plan:", &rows),
            vec![
                "Move plan:".to_string(),
                "  1. abc123def456 Add the thing (I0123456789a)".to_string(),
            ],
        );
    }

    #[test]
    fn cid_omitted_when_absent() {
        let rows = [row("abc123def4567890", "No change id", "", None)];
        assert_eq!(
            render_plan("Move plan:", &rows),
            vec![
                "Move plan:".to_string(),
                "  1. abc123def456 No change id".to_string(),
            ],
        );
    }

    #[test]
    fn action_tag_appended_when_present() {
        let rows = [row(
            "abc123def4567890",
            "Drop me",
            "I0123456789abcdef",
            Some("drop"),
        )];
        assert_eq!(
            render_plan("Drop plan:", &rows),
            vec![
                "Drop plan:".to_string(),
                "  1. abc123def456 Drop me (I0123456789a) [drop]".to_string(),
            ],
        );
    }

    #[test]
    fn action_tag_without_cid() {
        let rows = [row("abc123def4567890", "Drop me", "", Some("drop"))];
        assert_eq!(
            render_plan("Drop plan:", &rows),
            vec![
                "Drop plan:".to_string(),
                "  1. abc123def456 Drop me [drop]".to_string(),
            ],
        );
    }

    #[test]
    fn sha_and_change_id_truncated_to_twelve() {
        // Full 40-hex SHA and full I+40-hex Change-Id both truncate
        // to exactly 12 chars.
        let rows = [row(
            "0123456789abcdef0123456789abcdef01234567",
            "Long ids",
            "I0123456789abcdef0123456789abcdef01234567",
            Some("fixup"),
        )];
        assert_eq!(
            render_plan("Fixup plan:", &rows),
            vec![
                "Fixup plan:".to_string(),
                "  1. 0123456789ab Long ids (I0123456789a) [fixup]".to_string(),
            ],
        );
    }

    #[test]
    fn short_sha_and_cid_are_left_intact() {
        // Inputs shorter than 12 chars must not panic and pass
        // through unchanged.
        let rows = [row("abc", "Short", "I12", None)];
        assert_eq!(
            render_plan("Reorder plan:", &rows),
            vec![
                "Reorder plan:".to_string(),
                "  1. abc Short (I12)".to_string(),
            ],
        );
    }

    #[test]
    fn multi_row_numbering_is_one_based_and_sequential() {
        let rows = [
            row("aaaa111111111111", "First", "Iaaaa11112222", Some("fixup")),
            row("bbbb222222222222", "Second", "", None),
            row("cccc333333333333", "Third", "Icccc33334444", None),
        ];
        assert_eq!(
            render_plan("Squash plan:", &rows),
            vec![
                "Squash plan:".to_string(),
                "  1. aaaa11111111 First (Iaaaa1111222) [fixup]".to_string(),
                "  2. bbbb22222222 Second".to_string(),
                "  3. cccc33333333 Third (Icccc3333444)".to_string(),
            ],
        );
    }
}
