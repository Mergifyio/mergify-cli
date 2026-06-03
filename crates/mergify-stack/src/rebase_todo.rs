//! Rewrite the rebase-todo file `git rebase -i` would otherwise
//! open in `$GIT_EDITOR`.
//!
//! `mergify stack {edit,drop,reword,fixup,reorder,move,squash}`
//! all share the same pattern: spawn `git rebase -i <base>` with
//! `GIT_SEQUENCE_EDITOR` pointed at this binary; the binary's
//! `_internal rebase-todo-rewrite` subcommand reads the todo file,
//! applies one of the [`Action`] transformations defined here, and
//! writes it back. The pure transformer in this module keeps the
//! filesystem and process-spawning concerns out, so the parser is
//! exhaustively unit-testable without spawning git.
//!
//! Todo-line shape (from `git-rebase(1)`):
//! `<verb> <sha> <subject>`, e.g. `pick 1a2b3c4d feat: add foo`.
//! Blank lines and lines starting with `#` are kept verbatim
//! (they're comments / git-managed annotations and round-trip
//! intact in the rewritten file).

use mergify_core::CliError;

/// What [`rewrite`] should do with the targeted commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Mark `<sha>` as `edit` so the rebase pauses on that commit
    /// for an `git commit --amend` + `git rebase --continue` loop.
    /// Matching is prefix-based: a todo SHA either starts with
    /// `<sha>` or `<sha>` starts with the todo SHA.
    Edit { sha: String },
    /// Remove the `pick` lines for every SHA in `shas`. Each SHA
    /// must match exactly one `pick` line; missing matches surface
    /// as [`CliError::InvalidState`] so the rebase doesn't quietly
    /// proceed with a subset of the intended drops.
    Drop { shas: Vec<String> },
    /// Rewrite the `pick` lines for every SHA in `shas` as
    /// `fixup`. Same partial-match guard as [`Action::Drop`].
    Fixup { shas: Vec<String> },
}

/// Apply `action` to `todo` and return the rewritten contents.
/// Errors when the action doesn't match anything in `todo`
/// ([`CliError::InvalidState`]) so we fail loud instead of letting
/// the rebase proceed unchanged.
pub fn rewrite(todo: &str, action: &Action) -> Result<String, CliError> {
    match action {
        Action::Edit { sha } => rewrite_edit(todo, sha),
        Action::Drop { shas } => rewrite_drop(todo, shas),
        Action::Fixup { shas } => rewrite_replace_verb(todo, shas, "fixup"),
    }
}

fn rewrite_edit(todo: &str, target: &str) -> Result<String, CliError> {
    let mut matched = false;
    let mut out = String::with_capacity(todo.len());
    for line in todo.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if let Some(rest) = trimmed.strip_prefix("pick ") {
            // `<sha> <subject>` — pull the SHA off, leave the rest
            // alone so subjects with spaces survive.
            let (sha, _) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            if sha_matches(sha, target) {
                out.push_str("edit ");
                out.push_str(rest);
                // Preserve the original terminator (`\n` or
                // `\r\n`) — Windows checkouts can deliver CRLF.
                let terminator = &line[trimmed.len()..];
                out.push_str(terminator);
                matched = true;
                continue;
            }
        }
        out.push_str(line);
    }
    if !matched {
        return Err(CliError::InvalidState(format!(
            "rebase-todo has no `pick` line for {target}; aborting so the rebase doesn't run unchanged"
        )));
    }
    Ok(out)
}

fn rewrite_drop(todo: &str, targets: &[String]) -> Result<String, CliError> {
    if targets.is_empty() {
        return Err(CliError::InvalidState(
            "rebase-todo drop: no commits to drop".to_string(),
        ));
    }
    let mut matched: Vec<bool> = vec![false; targets.len()];
    let mut out = String::with_capacity(todo.len());
    for line in todo.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if let Some(rest) = trimmed.strip_prefix("pick ") {
            let (sha, _) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            if let Some(idx) = targets.iter().position(|t| sha_matches(sha, t)) {
                matched[idx] = true;
                // Skip this line — drop semantics is "remove the
                // pick line", same as deleting it in
                // `git rebase -i`.
                continue;
            }
        }
        out.push_str(line);
    }
    let missing: Vec<&str> = targets
        .iter()
        .zip(matched.iter())
        .filter_map(|(t, &m)| (!m).then_some(t.as_str()))
        .collect();
    if !missing.is_empty() {
        return Err(CliError::InvalidState(format!(
            "rebase-todo has no `pick` line for: {}; aborting so the rebase doesn't run with a partial drop",
            missing.join(", ")
        )));
    }
    Ok(out)
}

/// Generalised `pick → <verb>` rewriter for actions that swap the
/// command keyword instead of removing the line (today: `fixup`;
/// `reword` and friends slot in the same way). Every target must
/// match exactly one `pick` line; missing matches surface as
/// [`CliError::InvalidState`] so the rebase doesn't run with
/// only a subset of the intended changes.
fn rewrite_replace_verb(todo: &str, targets: &[String], verb: &str) -> Result<String, CliError> {
    if targets.is_empty() {
        return Err(CliError::InvalidState(format!(
            "rebase-todo {verb}: no commits to {verb}"
        )));
    }
    let mut matched: Vec<bool> = vec![false; targets.len()];
    let mut out = String::with_capacity(todo.len());
    for line in todo.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if let Some(rest) = trimmed.strip_prefix("pick ") {
            let (sha, _) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            if let Some(idx) = targets.iter().position(|t| sha_matches(sha, t)) {
                matched[idx] = true;
                out.push_str(verb);
                out.push(' ');
                out.push_str(rest);
                let terminator = &line[trimmed.len()..];
                out.push_str(terminator);
                continue;
            }
        }
        out.push_str(line);
    }
    let missing: Vec<&str> = targets
        .iter()
        .zip(matched.iter())
        .filter_map(|(t, &m)| (!m).then_some(t.as_str()))
        .collect();
    if !missing.is_empty() {
        return Err(CliError::InvalidState(format!(
            "rebase-todo has no `pick` line for: {}; aborting so the rebase doesn't run with a partial {verb}",
            missing.join(", ")
        )));
    }
    Ok(out)
}

/// True when *either* `todo_sha` or `target` is a prefix of the
/// other. Mirrors Python's
/// `target.startswith(parts[1]) or parts[1].startswith(target)`
/// so users can paste short or long SHAs from either direction.
fn sha_matches(todo_sha: &str, target: &str) -> bool {
    todo_sha.starts_with(target) || target.starts_with(todo_sha)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TODO: &str = "\
pick 1a2b3c4d feat: add foo
pick deadbeef chore: bump deps
pick cafe1234 fix: typo

# Rebase abc..def onto abc (3 commands)
";

    #[test]
    fn edit_marks_matching_pick() {
        let out = rewrite(
            TODO,
            &Action::Edit {
                sha: "deadbeef".to_string(),
            },
        )
        .unwrap();
        assert!(out.contains("edit deadbeef chore: bump deps\n"));
        // Other picks left alone.
        assert!(out.contains("pick 1a2b3c4d feat: add foo\n"));
        assert!(out.contains("pick cafe1234 fix: typo\n"));
        // Comment block preserved verbatim.
        assert!(out.contains("# Rebase abc..def onto abc (3 commands)\n"));
    }

    #[test]
    fn edit_matches_by_short_prefix() {
        // Long-target/short-todo case — covered by Python's
        // `target.startswith(parts[1])`.
        let todo = "pick abc12 feat\n";
        let out = rewrite(
            todo,
            &Action::Edit {
                sha: "abc1234567".to_string(),
            },
        )
        .unwrap();
        assert_eq!(out, "edit abc12 feat\n");
    }

    #[test]
    fn edit_matches_by_long_prefix() {
        // Short-target/long-todo case — covered by
        // `parts[1].startswith(target)`.
        let todo = "pick abc1234567 feat\n";
        let out = rewrite(
            todo,
            &Action::Edit {
                sha: "abc12".to_string(),
            },
        )
        .unwrap();
        assert_eq!(out, "edit abc1234567 feat\n");
    }

    #[test]
    fn edit_with_no_match_errors() {
        let err = rewrite(
            TODO,
            &Action::Edit {
                sha: "ffffffff".to_string(),
            },
        )
        .unwrap_err();
        match err {
            CliError::InvalidState(msg) => {
                assert!(msg.contains("ffffffff"), "got: {msg}");
                assert!(msg.contains("rebase-todo"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn edit_preserves_crlf_line_endings() {
        // Windows checkouts can hand us CRLF; the rewritten todo
        // must keep the same shape or git complains.
        let todo = "pick deadbeef chore: bump\r\n";
        let out = rewrite(
            todo,
            &Action::Edit {
                sha: "deadbeef".to_string(),
            },
        )
        .unwrap();
        assert_eq!(out, "edit deadbeef chore: bump\r\n");
    }

    #[test]
    fn edit_keeps_subjects_with_spaces() {
        let todo = "pick 1a2b3c4d feat: support remote/origin/HEAD lookups\n";
        let out = rewrite(
            todo,
            &Action::Edit {
                sha: "1a2b3c4d".to_string(),
            },
        )
        .unwrap();
        assert_eq!(
            out,
            "edit 1a2b3c4d feat: support remote/origin/HEAD lookups\n"
        );
    }

    #[test]
    fn non_pick_lines_are_left_alone() {
        // `git rebase -i --reschedule-failed-exec` etc. may seed
        // a todo with `exec`, `fixup`, `reword` lines. We only
        // touch `pick` lines.
        let todo = "\
pick 1a2b3c4d feat
fixup deadbeef hotfix
exec cargo test
";
        let out = rewrite(
            todo,
            &Action::Edit {
                sha: "1a2b3c4d".to_string(),
            },
        )
        .unwrap();
        assert_eq!(
            out,
            "edit 1a2b3c4d feat\nfixup deadbeef hotfix\nexec cargo test\n"
        );
    }

    #[test]
    fn drop_removes_targeted_pick_lines() {
        let out = rewrite(
            TODO,
            &Action::Drop {
                shas: vec!["deadbeef".to_string()],
            },
        )
        .unwrap();
        assert!(!out.contains("deadbeef"));
        assert!(out.contains("pick 1a2b3c4d feat: add foo\n"));
        assert!(out.contains("pick cafe1234 fix: typo\n"));
        assert!(out.contains("# Rebase abc..def onto abc (3 commands)\n"));
    }

    #[test]
    fn drop_handles_multiple_shas() {
        let out = rewrite(
            TODO,
            &Action::Drop {
                shas: vec!["1a2b3c4d".to_string(), "cafe1234".to_string()],
            },
        )
        .unwrap();
        assert!(!out.contains("1a2b3c4d"));
        assert!(!out.contains("cafe1234"));
        assert!(out.contains("pick deadbeef chore: bump deps\n"));
    }

    #[test]
    fn drop_with_no_match_errors() {
        let err = rewrite(
            TODO,
            &Action::Drop {
                shas: vec!["ffffffff".to_string()],
            },
        )
        .unwrap_err();
        match err {
            CliError::InvalidState(msg) => {
                assert!(msg.contains("ffffffff"), "got: {msg}");
                assert!(msg.contains("partial drop"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn drop_with_partial_match_errors_and_lists_missing() {
        // Half-and-half: one valid SHA, one missing. Python
        // skipped the valid drop and aborted — we do too, and we
        // name the missing ones to make the failure actionable.
        let err = rewrite(
            TODO,
            &Action::Drop {
                shas: vec!["deadbeef".to_string(), "ffffffff".to_string()],
            },
        )
        .unwrap_err();
        match err {
            CliError::InvalidState(msg) => {
                assert!(msg.contains("ffffffff"), "got: {msg}");
                assert!(!msg.contains("deadbeef"), "got: {msg}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn drop_with_empty_target_list_errors() {
        let err = rewrite(TODO, &Action::Drop { shas: vec![] }).unwrap_err();
        match err {
            CliError::InvalidState(msg) => {
                assert!(msg.contains("no commits to drop"), "got: {msg}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn fixup_rewrites_targeted_pick_to_fixup() {
        let out = rewrite(
            TODO,
            &Action::Fixup {
                shas: vec!["deadbeef".to_string()],
            },
        )
        .unwrap();
        assert!(out.contains("fixup deadbeef chore: bump deps\n"));
        assert!(out.contains("pick 1a2b3c4d feat: add foo\n"));
        assert!(out.contains("pick cafe1234 fix: typo\n"));
    }

    #[test]
    fn fixup_handles_multiple_shas() {
        let out = rewrite(
            TODO,
            &Action::Fixup {
                shas: vec!["1a2b3c4d".to_string(), "cafe1234".to_string()],
            },
        )
        .unwrap();
        assert!(out.contains("fixup 1a2b3c4d feat: add foo\n"));
        assert!(out.contains("fixup cafe1234 fix: typo\n"));
        assert!(out.contains("pick deadbeef chore: bump deps\n"));
    }

    #[test]
    fn fixup_with_no_match_errors() {
        let err = rewrite(
            TODO,
            &Action::Fixup {
                shas: vec!["ffffffff".to_string()],
            },
        )
        .unwrap_err();
        match err {
            CliError::InvalidState(msg) => {
                assert!(msg.contains("ffffffff"), "got: {msg}");
                assert!(msg.contains("partial fixup"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
