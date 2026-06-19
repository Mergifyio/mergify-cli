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
    /// Mark `<sha>` as `reword` so git stops at that commit and
    /// runs `git commit --amend`, opening `$GIT_EDITOR` for the
    /// message rewrite. Interactive; pair with an `-m` argument
    /// on the orchestrator to stay non-interactive (it'll use
    /// [`Action::ExecAfter`] instead).
    Reword { sha: String },
    /// Inject an `exec <command>` line right after the matching
    /// `pick` line. Used to run `git commit --amend -F <file>`
    /// while HEAD still points at the target commit (non-
    /// interactive reword), and as one half of `stack squash`'s
    /// custom-message path.
    ExecAfter { sha: String, command: String },
    /// Reorder the `pick` lines to the given sequence (other lines
    /// — comments, blank lines, `exec` etc. — are appended at the
    /// end). Every SHA in `ordered_shas` must match exactly one
    /// `pick` line and the count must equal the number of picks in
    /// the todo.
    Reorder { ordered_shas: Vec<String> },
    /// Combined reorder + fixup + optional exec-after used by
    /// `stack squash`. Each SHA in `ordered_shas` matches exactly
    /// one pick line; those listed in `fixup_shas` get their verb
    /// rewritten to `fixup`. If `exec_after_sha` and `exec_command`
    /// are both set, an `exec <command>` line is inserted right
    /// after the matching todo entry.
    Squash {
        ordered_shas: Vec<String>,
        fixup_shas: Vec<String>,
        exec_after_sha: Option<String>,
        exec_command: Option<String>,
    },
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
        Action::Reword { sha } => rewrite_replace_verb(todo, std::slice::from_ref(sha), "reword"),
        Action::ExecAfter { sha, command } => rewrite_exec_after(todo, sha, command),
        Action::Reorder { ordered_shas } => rewrite_reorder(todo, ordered_shas),
        Action::Squash {
            ordered_shas,
            fixup_shas,
            exec_after_sha,
            exec_command,
        } => rewrite_squash(
            todo,
            ordered_shas,
            fixup_shas,
            exec_after_sha.as_deref(),
            exec_command.as_deref(),
        ),
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
/// match exactly one `pick` line — missing matches surface as
/// [`CliError::InvalidState`] so the rebase doesn't run with only
/// a subset of the intended changes, and ambiguous matches (a
/// SHA prefix that resolves to multiple `pick` lines) error out
/// for the same reason: silently rewriting two commits when the
/// caller meant one would corrupt the rebase.
fn rewrite_replace_verb(todo: &str, targets: &[String], verb: &str) -> Result<String, CliError> {
    if targets.is_empty() {
        return Err(CliError::InvalidState(format!(
            "rebase-todo {verb}: no commits to {verb}"
        )));
    }
    let mut match_count: Vec<usize> = vec![0; targets.len()];
    let mut out = String::with_capacity(todo.len());
    for line in todo.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if let Some(rest) = trimmed.strip_prefix("pick ") {
            let (sha, _) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            if let Some(idx) = targets.iter().position(|t| sha_matches(sha, t)) {
                match_count[idx] += 1;
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
    let ambiguous: Vec<&str> = targets
        .iter()
        .zip(match_count.iter())
        .filter_map(|(t, &n)| (n > 1).then_some(t.as_str()))
        .collect();
    if !ambiguous.is_empty() {
        return Err(CliError::InvalidState(format!(
            "rebase-todo {verb}: target(s) {} matched multiple `pick` lines; pass a longer SHA prefix",
            ambiguous.join(", "),
        )));
    }
    let matched: Vec<bool> = match_count.iter().map(|&n| n > 0).collect();
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

fn rewrite_exec_after(todo: &str, target: &str, command: &str) -> Result<String, CliError> {
    let mut matched = false;
    let mut out = String::with_capacity(todo.len() + command.len() + 8);
    for line in todo.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        out.push_str(line);
        if matched {
            continue;
        }
        // Match against the *current* line — if it's a pick for the
        // target, emit the `exec` line right after.
        if let Some(rest) = trimmed.strip_prefix("pick ") {
            let (sha, _) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            if sha_matches(sha, target) {
                let terminator = &line[trimmed.len()..];
                out.push_str("exec ");
                out.push_str(command);
                out.push_str(if terminator.is_empty() {
                    "\n"
                } else {
                    terminator
                });
                matched = true;
            }
        }
    }
    if !matched {
        return Err(CliError::InvalidState(format!(
            "rebase-todo has no `pick` line for {target}; aborting so the exec doesn't run unanchored"
        )));
    }
    Ok(out)
}

/// Reorder the `pick` lines to match `ordered_shas`. Non-pick
/// lines (comments, blank lines, `exec` annotations) are
/// preserved verbatim at the *end* of the rewritten todo, after
/// the new pick order. Mirrors the Python `run_action_rebase`
/// fallback that bucketed "other" lines after the reordered
/// picks.
fn rewrite_reorder(todo: &str, ordered_shas: &[String]) -> Result<String, CliError> {
    // First pass: split into pick lines (keyed by SHA) and "other".
    let mut pick_lines: Vec<(String, String)> = Vec::new();
    let mut other_lines: String = String::new();
    for line in todo.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if let Some(rest) = trimmed.strip_prefix("pick ") {
            let (sha, _) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            pick_lines.push((sha.to_string(), line.to_string()));
        } else {
            other_lines.push_str(line);
        }
    }
    if ordered_shas.len() != pick_lines.len() {
        return Err(CliError::InvalidState(format!(
            "rebase-todo reorder: have {} pick lines but caller asked for {} ordered SHAs",
            pick_lines.len(),
            ordered_shas.len()
        )));
    }
    // Second pass: rebuild in the requested order, consuming each
    // pick at most once so duplicates surface.
    let mut consumed = vec![false; pick_lines.len()];
    let mut out = String::with_capacity(todo.len());
    for sha in ordered_shas {
        let idx = pick_lines
            .iter()
            .enumerate()
            .position(|(i, (todo_sha, _))| !consumed[i] && sha_matches(todo_sha, sha));
        let Some(idx) = idx else {
            return Err(CliError::InvalidState(format!(
                "rebase-todo reorder: no remaining pick line matches {sha}"
            )));
        };
        consumed[idx] = true;
        out.push_str(&pick_lines[idx].1);
    }
    out.push_str(&other_lines);
    Ok(out)
}

/// Combined reorder + per-SHA verb swap + optional `exec` line.
/// Mirrors Python's `run_action_rebase` with the same call shape
/// the `stack squash` orchestrator uses.
fn rewrite_squash(
    todo: &str,
    ordered_shas: &[String],
    fixup_shas: &[String],
    exec_after_sha: Option<&str>,
    exec_command: Option<&str>,
) -> Result<String, CliError> {
    // Split todo into picks (keyed by SHA) and other lines (kept
    // verbatim at the end, same bucketing as Action::Reorder).
    let mut pick_lines: Vec<(String, String)> = Vec::new();
    let mut other_lines = String::new();
    for line in todo.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if let Some(rest) = trimmed.strip_prefix("pick ") {
            let (sha, _) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            pick_lines.push((sha.to_string(), line.to_string()));
        } else {
            other_lines.push_str(line);
        }
    }
    if ordered_shas.len() != pick_lines.len() {
        return Err(CliError::InvalidState(format!(
            "rebase-todo squash: have {} pick lines but caller asked for {} ordered SHAs",
            pick_lines.len(),
            ordered_shas.len()
        )));
    }

    let exec_set = exec_after_sha.zip(exec_command);
    let mut consumed = vec![false; pick_lines.len()];
    let mut out = String::with_capacity(todo.len());
    for sha in ordered_shas {
        let idx = pick_lines
            .iter()
            .enumerate()
            .position(|(i, (todo_sha, _))| !consumed[i] && sha_matches(todo_sha, sha));
        let Some(idx) = idx else {
            return Err(CliError::InvalidState(format!(
                "rebase-todo squash: no remaining pick line matches {sha}"
            )));
        };
        consumed[idx] = true;
        let original_line = &pick_lines[idx].1;
        let trimmed = original_line.trim_end_matches(['\n', '\r']);
        let rest = trimmed.strip_prefix("pick ").unwrap_or(trimmed);
        let terminator = &original_line[trimmed.len()..];
        let term_to_emit = if terminator.is_empty() {
            "\n"
        } else {
            terminator
        };

        let verb = if fixup_shas.iter().any(|f| sha_matches(sha, f)) {
            "fixup"
        } else {
            "pick"
        };
        out.push_str(verb);
        out.push(' ');
        out.push_str(rest);
        out.push_str(term_to_emit);

        if let Some((after_sha, command)) = exec_set
            && sha_matches(sha, after_sha)
        {
            out.push_str("exec ");
            out.push_str(command);
            out.push_str(term_to_emit);
        }
    }
    out.push_str(&other_lines);
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

    #[test]
    fn reword_marks_matching_pick() {
        let out = rewrite(
            TODO,
            &Action::Reword {
                sha: "deadbeef".to_string(),
            },
        )
        .unwrap();
        assert!(out.contains("reword deadbeef chore: bump deps\n"));
        assert!(out.contains("pick 1a2b3c4d feat: add foo\n"));
    }

    #[test]
    fn reword_with_no_match_errors() {
        let err = rewrite(
            TODO,
            &Action::Reword {
                sha: "ffffffff".to_string(),
            },
        )
        .unwrap_err();
        match err {
            CliError::InvalidState(msg) => assert!(msg.contains("partial reword"), "got: {msg}"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn exec_after_injects_line_below_target() {
        let out = rewrite(
            TODO,
            &Action::ExecAfter {
                sha: "deadbeef".to_string(),
                command: "git commit --amend -F /tmp/msg.txt".to_string(),
            },
        )
        .unwrap();
        assert_eq!(
            out,
            "pick 1a2b3c4d feat: add foo\n\
             pick deadbeef chore: bump deps\n\
             exec git commit --amend -F /tmp/msg.txt\n\
             pick cafe1234 fix: typo\n\
             \n\
             # Rebase abc..def onto abc (3 commands)\n"
        );
    }

    #[test]
    fn reorder_rewrites_picks_in_given_order() {
        let out = rewrite(
            TODO,
            &Action::Reorder {
                ordered_shas: vec![
                    "cafe1234".to_string(),
                    "1a2b3c4d".to_string(),
                    "deadbeef".to_string(),
                ],
            },
        )
        .unwrap();
        assert_eq!(
            out,
            "pick cafe1234 fix: typo\n\
             pick 1a2b3c4d feat: add foo\n\
             pick deadbeef chore: bump deps\n\
             \n\
             # Rebase abc..def onto abc (3 commands)\n"
        );
    }

    #[test]
    fn reorder_count_mismatch_errors() {
        let err = rewrite(
            TODO,
            &Action::Reorder {
                ordered_shas: vec!["1a2b3c4d".to_string()],
            },
        )
        .unwrap_err();
        match err {
            CliError::InvalidState(msg) => {
                assert!(
                    msg.contains("3 pick lines") && msg.contains("1 ordered"),
                    "got: {msg}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn squash_folds_srcs_into_target_and_keeps_target_verb() {
        // ordered: target then src; src is fixup so it folds in.
        let out = rewrite(
            TODO,
            &Action::Squash {
                ordered_shas: vec![
                    "1a2b3c4d".to_string(), // A
                    "cafe1234".to_string(), // C (will be fixed up into A)
                    "deadbeef".to_string(), // B (stays a pick)
                ],
                fixup_shas: vec!["cafe1234".to_string()],
                exec_after_sha: None,
                exec_command: None,
            },
        )
        .unwrap();
        assert_eq!(
            out,
            "pick 1a2b3c4d feat: add foo\n\
             fixup cafe1234 fix: typo\n\
             pick deadbeef chore: bump deps\n\
             \n\
             # Rebase abc..def onto abc (3 commands)\n"
        );
    }

    #[test]
    fn squash_with_exec_after_injects_command() {
        let out = rewrite(
            TODO,
            &Action::Squash {
                ordered_shas: vec![
                    "1a2b3c4d".to_string(),
                    "cafe1234".to_string(),
                    "deadbeef".to_string(),
                ],
                fixup_shas: vec!["cafe1234".to_string()],
                exec_after_sha: Some("cafe1234".to_string()),
                exec_command: Some("git commit --amend -F /tmp/msg.txt".to_string()),
            },
        )
        .unwrap();
        assert_eq!(
            out,
            "pick 1a2b3c4d feat: add foo\n\
             fixup cafe1234 fix: typo\n\
             exec git commit --amend -F /tmp/msg.txt\n\
             pick deadbeef chore: bump deps\n\
             \n\
             # Rebase abc..def onto abc (3 commands)\n"
        );
    }

    #[test]
    fn squash_count_mismatch_errors() {
        let err = rewrite(
            TODO,
            &Action::Squash {
                ordered_shas: vec!["1a2b3c4d".to_string()],
                fixup_shas: vec![],
                exec_after_sha: None,
                exec_command: None,
            },
        )
        .unwrap_err();
        match err {
            CliError::InvalidState(msg) => {
                assert!(msg.contains("3 pick lines"), "got: {msg}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn reorder_unknown_sha_errors() {
        let err = rewrite(
            TODO,
            &Action::Reorder {
                ordered_shas: vec![
                    "ffffffff".to_string(),
                    "1a2b3c4d".to_string(),
                    "deadbeef".to_string(),
                ],
            },
        )
        .unwrap_err();
        match err {
            CliError::InvalidState(msg) => assert!(msg.contains("ffffffff"), "got: {msg}"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn exec_after_with_no_match_errors() {
        let err = rewrite(
            TODO,
            &Action::ExecAfter {
                sha: "ffffffff".to_string(),
                command: "true".to_string(),
            },
        )
        .unwrap_err();
        match err {
            CliError::InvalidState(msg) => {
                assert!(msg.contains("ffffffff"), "got: {msg}");
                assert!(msg.contains("unanchored"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
