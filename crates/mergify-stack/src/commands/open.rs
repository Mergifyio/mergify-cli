//! `mergify stack open [<commit>]` — open the PR for a stack
//! commit in the user's default browser.
//!
//! Port of `mergify_cli/stack/open.py::stack_open`. Walks the
//! current stack (via the same machinery `stack list` uses with
//! `include_status=false`), resolves the target commit, then
//! hands the PR URL to the OS's URL-launcher (`open` on macOS,
//! `xdg-open` on Linux, `cmd /C start` on Windows).

use std::path::Path;
use std::process::Command;

use mergify_core::CliError;
use mergify_core::HttpClient;

use crate::commands::list::{self, StackListEntry};
use crate::trunk;

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
        // Default to the leaf — Python's interactive picker
        // defaulted to it too, and the explicit-commit path is
        // what test/automation users hit. The interactive picker
        // itself is left out of the port for now.
        None => stack.entries.last().cloned().expect("non-empty"),
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

// Currently unused; kept for tests that may want to exercise the
// stack-walk path without mocking the URL opener.
#[allow(dead_code)]
fn touch(_: &trunk::Trunk) {}
