//! `mergify stack squash <SRC>... into <TARGET> [-m <msg>]
//! [--dry-run]` — combine several commits into a target.
//!
//! Port of `mergify_cli/stack/squash.py::stack_squash`. Reorders
//! every SRC adjacent to TARGET and rewrites the SRC verbs as
//! `fixup` (so they fold without opening an editor and TARGET's
//! message survives). When `-m` is given, the new combined
//! message is applied via an `exec git commit --amend -F <file>`
//! line inserted right after the last fixed-up commit — same
//! tempfile-leak pattern as `stack reword -m`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use mergify_core::CliError;

use crate::change_id;
use crate::local_commits::{self, LocalCommit};
use crate::trunk;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderedCommit {
    pub sha: String,
    pub subject: String,
}

#[derive(Debug, Clone)]
pub enum Outcome {
    Squashed { plan: Vec<OrderedCommit> },
    DryRun { plan: Vec<OrderedCommit> },
    EmptyStack,
}

pub struct Options<'a> {
    pub repo_dir: Option<&'a Path>,
    pub src_prefixes: &'a [String],
    pub target_prefix: &'a str,
    pub message: Option<&'a str>,
    pub dry_run: bool,
    pub mergify_binary: &'a Path,
}

pub fn run(opts: &Options<'_>) -> Result<Outcome, CliError> {
    if opts.src_prefixes.is_empty() {
        return Err(CliError::InvalidState(
            "at least one source commit required".to_string(),
        ));
    }
    let repo_dir = resolve_repo_toplevel(opts.repo_dir)?;
    let trunk = trunk::get_trunk(Some(&repo_dir)).map_err(|e| {
        CliError::StackNotFound(format!(
            "could not determine trunk branch ({e}). Please set \
             upstream tracking or set a base manually."
        ))
    })?;
    let base = run_git_capture(Some(&repo_dir), &["merge-base", &trunk.refspec(), "HEAD"])?;
    let commits = local_commits::read(&repo_dir, &base, "HEAD")?;
    if commits.is_empty() {
        return Ok(Outcome::EmptyStack);
    }

    let target = match_commit(opts.target_prefix, &commits)?;

    let mut srcs: Vec<OrderedCommit> = Vec::with_capacity(opts.src_prefixes.len());
    let mut seen_src = std::collections::HashSet::new();
    for prefix in opts.src_prefixes {
        let matched = match_commit(prefix, &commits)?;
        if matched.sha == target.sha {
            return Err(CliError::InvalidState(
                "a source commit cannot be the same as the target".to_string(),
            ));
        }
        if !seen_src.insert(matched.sha.clone()) {
            return Err(CliError::InvalidState(format!(
                "duplicate — source prefix '{prefix}' resolves to the same commit as another"
            )));
        }
        srcs.push(matched);
    }

    // Build new order: non-src commits in their original order,
    // with the src list reinserted immediately after target.
    let src_sha_set: std::collections::HashSet<&str> =
        srcs.iter().map(|s| s.sha.as_str()).collect();
    let mut new_order: Vec<OrderedCommit> = Vec::with_capacity(commits.len());
    for c in &commits {
        if src_sha_set.contains(c.commit_sha.as_str()) {
            continue;
        }
        new_order.push(OrderedCommit {
            sha: c.commit_sha.clone(),
            subject: c.title.clone(),
        });
        if c.commit_sha == target.sha {
            new_order.extend(srcs.iter().cloned());
        }
    }

    if opts.dry_run {
        return Ok(Outcome::DryRun { plan: new_order });
    }

    let ordered_shas: Vec<String> = new_order.iter().map(|c| c.sha.clone()).collect();
    let fixup_shas: Vec<String> = srcs.iter().map(|s| s.sha.clone()).collect();
    let (exec_after_sha, exec_command) = if let Some(msg) = opts.message {
        let msg_path = write_temp_message(msg)?;
        let command = format!(
            "git commit --amend -F {}",
            shell_quote(&msg_path.to_string_lossy())
        );
        let last_src = srcs.last().expect("non-empty validated above").sha.clone();
        (Some(last_src), Some(command))
    } else {
        (None, None)
    };

    spawn_squash_rebase(
        &repo_dir,
        &base,
        opts.mergify_binary,
        &ordered_shas,
        &fixup_shas,
        exec_after_sha.as_deref(),
        exec_command.as_deref(),
    )?;
    Ok(Outcome::Squashed { plan: new_order })
}

fn match_commit(prefix: &str, commits: &[LocalCommit]) -> Result<OrderedCommit, CliError> {
    let (matches, field): (Vec<&LocalCommit>, &str) = if change_id::is_prefix(prefix) {
        (
            commits
                .iter()
                .filter(|c| c.change_id.starts_with(prefix))
                .collect(),
            "Change-Id",
        )
    } else {
        (
            commits
                .iter()
                .filter(|c| c.commit_sha.starts_with(prefix))
                .collect(),
            "SHA",
        )
    };
    match matches.as_slice() {
        [] => Err(CliError::StackNotFound(format!(
            "no commit found matching {field} prefix '{prefix}'"
        ))),
        [only] => Ok(OrderedCommit {
            sha: only.commit_sha.clone(),
            subject: only.title.clone(),
        }),
        many => {
            let listing = many
                .iter()
                .map(|c| format!("{} {}", &c.commit_sha[..7], c.title))
                .collect::<Vec<_>>()
                .join("\n  ");
            Err(CliError::InvalidState(format!(
                "{field} prefix '{prefix}' matches multiple commits:\n  {listing}"
            )))
        }
    }
}

fn write_temp_message(message: &str) -> Result<PathBuf, CliError> {
    let mut tmp = tempfile::Builder::new()
        .prefix("mergify_squash_msg_")
        .suffix(".txt")
        .tempfile()
        .map_err(|e| CliError::Generic(format!("create squash tempfile: {e}")))?;
    tmp.write_all(message.as_bytes())
        .map_err(|e| CliError::Generic(format!("write squash tempfile: {e}")))?;
    tmp.flush()
        .map_err(|e| CliError::Generic(format!("flush squash tempfile: {e}")))?;
    let (_, path) = tmp
        .keep()
        .map_err(|e| CliError::Generic(format!("persist squash tempfile: {e}")))?;
    Ok(path)
}

fn spawn_squash_rebase(
    repo_dir: &Path,
    base: &str,
    mergify_binary: &Path,
    ordered_shas: &[String],
    fixup_shas: &[String],
    exec_after_sha: Option<&str>,
    exec_command: Option<&str>,
) -> Result<(), CliError> {
    let bin = shell_quote(&mergify_binary.to_string_lossy());
    let ordered = shell_quote(&ordered_shas.join(","));
    let fixup = shell_quote(&fixup_shas.join(","));
    let mut editor = format!(
        "{bin} _internal rebase-todo-rewrite --action squash --shas {ordered} --fixup-shas {fixup}"
    );
    if let (Some(after), Some(cmd)) = (exec_after_sha, exec_command) {
        editor.push_str(" --sha ");
        editor.push_str(&shell_quote(after));
        editor.push_str(" --command ");
        editor.push_str(&shell_quote(cmd));
    }
    spawn_rebase(repo_dir, base, &editor)
}

fn shell_quote(value: &str) -> String {
    let escaped = value.replace('\'', "'\\''");
    format!("'{escaped}'")
}

fn resolve_repo_toplevel(repo_dir: Option<&Path>) -> Result<PathBuf, CliError> {
    let raw = run_git_capture(repo_dir, &["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(raw))
}

fn spawn_rebase(repo_dir: &Path, base: &str, sequence_editor: &str) -> Result<(), CliError> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args(["rebase", "-i", base])
        .env("GIT_SEQUENCE_EDITOR", sequence_editor)
        .status()
        .map_err(|e| CliError::Generic(format!("failed to spawn `git rebase -i`: {e}")))?;
    if !status.success() {
        return Err(CliError::Generic(format!(
            "`git rebase -i {base}` exited {status}"
        )));
    }
    Ok(())
}

fn run_git_capture(repo_dir: Option<&Path>, args: &[&str]) -> Result<String, CliError> {
    let mut cmd = Command::new("git");
    if let Some(dir) = repo_dir {
        cmd.arg("-C").arg(dir);
    }
    cmd.args(args);
    let output = cmd
        .output()
        .map_err(|e| CliError::Generic(format!("failed to spawn `git {}`: {e}", args.join(" "))))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(CliError::Generic(if stderr.is_empty() {
            format!("`git {}` failed", args.join(" "))
        } else {
            stderr
        }));
    }
    let stdout = String::from_utf8(output.stdout).map_err(|e| {
        CliError::Generic(format!("`git {}` output is not UTF-8: {e}", args.join(" ")))
    })?;
    Ok(stdout.trim_end().to_string())
}
