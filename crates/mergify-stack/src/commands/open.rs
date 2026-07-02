//! `mergify stack open [<commit>]` — open the PR for a stack
//! commit in the user's default browser.
//!
//! Walks the current stack (via the same machinery `stack list`
//! uses with `include_status=false`), resolves the target commit,
//! then hands the PR URL to the OS's URL-launcher (`open` on
//! macOS, `xdg-open` on Linux, `cmd /C start` on Windows).
//!
//! With no `<commit>` argument the binary injects an interactive
//! fuzzy picker (see [`Selector`]) when stdin and stdout are TTYs;
//! otherwise the leaf (HEAD) is opened, which non-TTY callers and
//! scripts rely on.

use std::path::Path;
use std::process::Command;

use mergify_core::CliError;
use mergify_core::HttpClient;

use crate::commands::list::{self, StackListEntry};

/// Interactive selection hook: given picker labels and the index
/// to preselect, yield the chosen index (`None` = user cancelled).
/// Injected by the binary — which wires `mergify_tui::fuzzy_select`
/// — so this crate stays testable without a TTY, the same
/// testability-by-parameter pattern as `queue pause`'s `confirm`.
pub type Selector<'a> = &'a dyn Fn(&[String], usize) -> std::io::Result<Option<usize>>;

#[derive(Debug, Clone)]
pub enum Outcome {
    /// PR URL was passed to the OS opener.
    Opened {
        pull_number: u64,
        title: String,
        pull_url: String,
    },
    /// Stack has no commits — nothing to open.
    EmptyStack,
    /// Interactive picker dismissed (Escape / Ctrl-C). Not an error:
    /// the binary prints nothing and exits 0, matching Python.
    Cancelled,
}

pub struct Options<'a> {
    pub repo_dir: Option<&'a Path>,
    pub client: &'a HttpClient,
    pub user: &'a str,
    pub repo: &'a str,
    pub author: &'a str,
    pub branch_prefix: &'a str,
    pub trunk: (&'a str, &'a str),
    /// Commit selector. `None` defaults to the leaf (HEAD).
    /// Accepts a full SHA, a SHA prefix, or any git ref the
    /// repo understands (`HEAD~1` etc.).
    pub commit: Option<&'a str>,
    /// Interactive picker used when `commit` is `None`. `None` here
    /// (non-TTY callers, tests) falls back to opening the leaf.
    pub selector: Option<Selector<'a>>,
}

pub async fn run(opts: &Options<'_>) -> Result<Outcome, CliError> {
    let stack = list::run(&list::Options {
        repo_dir: opts.repo_dir,
        client: opts.client,
        user: opts.user,
        repo: opts.repo,
        author: opts.author,
        branch_prefix: opts.branch_prefix,
        trunk: opts.trunk,
        // `stack open` doesn't render CI / review status, so skip
        // the per-PR fetches to keep the latency low.
        include_status: false,
    })
    .await?;

    if stack.entries.is_empty() {
        return Ok(Outcome::EmptyStack);
    }

    let entry = match opts.commit {
        None => match opts.selector {
            Some(selector) => match pick_interactive(&stack.entries, selector)? {
                Some(entry) => entry,
                None => return Ok(Outcome::Cancelled),
            },
            // Leaf default: pre-picker port behavior, kept for
            // non-TTY callers and scripts.
            None => stack.entries.last().cloned().expect("non-empty"),
        },
        Some(commit) => resolve_entry(opts.repo_dir, &stack.entries, commit)?,
    };

    let pull_url = entry.pull_url.ok_or_else(|| {
        CliError::StackNotFound(format!(
            "No PR for: {title} ({short}) — run `mergify stack push` first.",
            title = entry.title,
            short = &entry.commit_sha[..entry.commit_sha.len().min(7)],
        ))
    })?;
    let pull_number = entry
        .pull_number
        .ok_or_else(|| CliError::Generic("entry has pull_url but no pull_number".to_string()))?;

    spawn_opener(&pull_url)?;

    Ok(Outcome::Opened {
        pull_number,
        title: entry.title,
        pull_url,
    })
}

/// Picker labels, one per stack entry: `#<PR> <title> (<sha7>)`,
/// or `(no PR) <title> (<sha7>)` for commits not pushed yet. The
/// no-PR entries stay listed so the picker shows the whole stack
/// shape; selecting one fails downstream with the same "push
/// first" error as the explicit-commit path.
fn build_labels(entries: &[StackListEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| {
            let short = &entry.commit_sha[..entry.commit_sha.len().min(7)];
            match entry.pull_number {
                Some(number) => format!("#{number} {title} ({short})", title = entry.title),
                None => format!("(no PR) {title} ({short})", title = entry.title),
            }
        })
        .collect()
}

/// Run the injected picker over the stack, leaf preselected.
/// `Ok(None)` means the user cancelled. Callers guarantee
/// `entries` is non-empty.
fn pick_interactive(
    entries: &[StackListEntry],
    selector: Selector<'_>,
) -> Result<Option<StackListEntry>, CliError> {
    let labels = build_labels(entries);
    let picked = selector(&labels, labels.len().saturating_sub(1))
        .map_err(|e| CliError::Generic(format!("interactive selection failed: {e}")))?;
    Ok(picked.map(|index| entries[index].clone()))
}

fn resolve_entry(
    repo_dir: Option<&Path>,
    entries: &[StackListEntry],
    commit: &str,
) -> Result<StackListEntry, CliError> {
    let resolved = resolve_ref_to_sha(repo_dir, commit)?;
    entries
        .iter()
        .find(|e| e.commit_sha == resolved)
        .cloned()
        .ok_or_else(|| {
            CliError::StackNotFound(format!(
                "commit `{commit}` ({short}) not found in stack",
                short = &resolved[..resolved.len().min(7)],
            ))
        })
}

fn resolve_ref_to_sha(repo_dir: Option<&Path>, commit: &str) -> Result<String, CliError> {
    let mut cmd = Command::new("git");
    if let Some(dir) = repo_dir {
        cmd.arg("-C").arg(dir);
    }
    cmd.args(["rev-parse", "--verify", commit]);
    let out = cmd
        .output()
        .map_err(|e| CliError::Generic(format!("spawn git rev-parse: {e}")))?;
    if !out.status.success() {
        return Err(CliError::StackNotFound(format!(
            "commit `{commit}` not found"
        )));
    }
    let sha = String::from_utf8(out.stdout)
        .map_err(|e| CliError::Generic(format!("git output not UTF-8: {e}")))?
        .trim()
        .to_string();
    if sha.is_empty() {
        return Err(CliError::StackNotFound(format!(
            "commit `{commit}` resolves to empty SHA"
        )));
    }
    Ok(sha)
}

#[cfg(target_os = "macos")]
fn spawn_opener(url: &str) -> Result<(), CliError> {
    Command::new("open")
        .arg(url)
        .status()
        .map_err(|e| CliError::Generic(format!("failed to spawn `open`: {e}")))?;
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn spawn_opener(url: &str) -> Result<(), CliError> {
    Command::new("xdg-open")
        .arg(url)
        .status()
        .map_err(|e| CliError::Generic(format!("failed to spawn `xdg-open`: {e}")))?;
    Ok(())
}

#[cfg(windows)]
fn spawn_opener(url: &str) -> Result<(), CliError> {
    // `start` is a cmd.exe builtin; the empty `""` placeholder
    // tells start the first arg isn't a window title.
    Command::new("cmd")
        .args(["/C", "start", "", url])
        .status()
        .map_err(|e| CliError::Generic(format!("failed to spawn `start`: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(title: &str, sha: &str, pull: Option<(u64, &str)>) -> StackListEntry {
        StackListEntry {
            commit_sha: sha.to_string(),
            title: title.to_string(),
            change_id: String::new(),
            status: pull.map_or("no_pr", |_| "open").to_string(),
            pull_number: pull.map(|(number, _)| number),
            pull_url: pull.map(|(_, url)| url.to_string()),
            ci_status: String::new(),
            ci_checks: Vec::new(),
            review_status: String::new(),
            reviews: Vec::new(),
            mergeable: None,
        }
    }

    fn two_entries() -> Vec<StackListEntry> {
        vec![
            entry(
                "feat: add API endpoint",
                "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678",
                Some((101, "https://github.com/o/r/pull/101")),
            ),
            entry(
                "wip: not pushed yet",
                "0123456789abcdef0123456789abcdef01234567",
                None,
            ),
        ]
    }

    #[test]
    fn labels_show_pr_number_or_no_pr_marker() {
        assert_eq!(
            build_labels(&two_entries()),
            vec![
                "#101 feat: add API endpoint (a1b2c3d)".to_string(),
                "(no PR) wip: not pushed yet (0123456)".to_string(),
            ],
        );
    }

    #[test]
    fn picker_preselects_the_leaf_and_returns_the_picked_entry() {
        let entries = two_entries();
        let seen_default = std::cell::Cell::new(usize::MAX);
        let selector = |labels: &[String], default: usize| {
            assert_eq!(labels.len(), 2);
            seen_default.set(default);
            Ok(Some(0))
        };
        let picked = pick_interactive(&entries, &selector).unwrap().unwrap();
        // Leaf (last entry) is the preselected default…
        assert_eq!(seen_default.get(), 1);
        // …but the selector's answer wins.
        assert_eq!(picked.commit_sha, entries[0].commit_sha);
    }

    #[test]
    fn cancel_yields_none() {
        let selector = |_: &[String], _: usize| Ok(None);
        assert!(
            pick_interactive(&two_entries(), &selector)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn selector_io_error_becomes_cli_error() {
        let selector = |_: &[String], _: usize| Err(std::io::Error::other("terminal exploded"));
        let err = pick_interactive(&two_entries(), &selector).unwrap_err();
        assert!(matches!(err, CliError::Generic(_)));
    }
}
