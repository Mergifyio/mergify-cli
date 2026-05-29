//! `git diff` between two refs, with progressive deepening of a
//! shallow clone if no merge base exists yet.
//!
//! Mirrors `mergify_cli/ci/scopes/changed_files.py`. The history
//! deepening is necessary in CI: GitHub Actions checkouts default
//! to depth=1, and the merge base between `base` and `head`
//! probably lives further back. We fetch in batches of 100
//! commits until either a merge base appears or the commit count
//! stops growing (meaning we've reached the root and there's
//! genuinely no common ancestor).

use std::process::Command;

use mergify_core::CliError;

/// Scoped namespace for refs we fetch ourselves, to avoid
/// clashing with `refs/remotes/origin/*` (which may not exist or
/// may point elsewhere).
const FETCHED_REF_PREFIX: &str = "refs/mergify-cli/fetched/";

const COMMITS_BATCH_SIZE: u64 = 100;

fn is_sha(ref_: &str) -> bool {
    // Only full 40-char SHAs — abbreviated SHAs would false-match
    // branch names like "deadbeef" and cause `git fetch` to treat
    // them as branches.
    ref_.len() == 40
        && ref_
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn is_local_ref(ref_: &str) -> bool {
    ref_ == "HEAD" || ref_.starts_with("HEAD~") || ref_.starts_with("HEAD^")
}

fn local_ref(ref_: &str) -> String {
    if is_sha(ref_) || is_local_ref(ref_) {
        ref_.to_string()
    } else {
        format!("{FETCHED_REF_PREFIX}{ref_}")
    }
}

fn fetch_arg(ref_: &str) -> Option<String> {
    if is_local_ref(ref_) {
        None
    } else if is_sha(ref_) {
        Some(ref_.to_string())
    } else {
        // `git fetch origin <branch>` only updates `FETCH_HEAD`;
        // use an explicit refspec so the branch becomes a real
        // local ref we can name later.
        Some(format!("+{ref_}:{}", local_ref(ref_)))
    }
}

fn run_git(args: &[&str]) -> Result<String, CliError> {
    let out = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| CliError::Generic(format!("failed to spawn `git {args:?}`: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(CliError::Generic(format!(
            "`git {}` failed ({}): {}",
            args.join(" "),
            out.status,
            stderr.trim(),
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn has_merge_base(base: &str, head: &str) -> bool {
    Command::new("git")
        .args(["merge-base", "--", base, head])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn commits_count() -> Result<u64, CliError> {
    let out = run_git(&["rev-list", "--count", "--all"])?;
    out.parse::<u64>()
        .map_err(|e| CliError::Generic(format!("could not parse commit count {out:?}: {e}")))
}

fn fetch(depth_flag: &str, fetch_args: &[String]) -> Result<(), CliError> {
    let mut args: Vec<&str> = vec!["fetch", "--no-tags", depth_flag, "origin"];
    if !fetch_args.is_empty() {
        args.push("--");
        for fa in fetch_args {
            args.push(fa);
        }
    }
    run_git(&args).map(drop)
}

/// Deepen the local clone until `base` and `head` share an
/// ancestor (or until we've exhausted history). Returns the
/// pair of local ref names (`refs/mergify-cli/fetched/<ref>` for
/// remote names; `HEAD~N` / `HEAD^N` / SHA passed through
/// untouched) that the subsequent `git diff` should target.
pub fn ensure_history(base: &str, head: &str) -> Result<(String, String), CliError> {
    if has_merge_base(base, head) {
        return Ok((base.to_string(), head.to_string()));
    }

    let fetch_args: Vec<String> = [fetch_arg(base), fetch_arg(head)]
        .into_iter()
        .flatten()
        .collect();
    let local_base = local_ref(base);
    let local_head = local_ref(head);
    let mut depth = COMMITS_BATCH_SIZE;

    fetch(&format!("--depth={depth}"), &fetch_args)?;

    let mut last_count = commits_count()?;
    while !has_merge_base(&local_base, &local_head) {
        depth = depth.saturating_mul(2);
        fetch(&format!("--deepen={depth}"), &fetch_args)?;
        let count = commits_count()?;
        if count == last_count {
            // No new commits this round — we've reached the root
            // and the refs genuinely have no common ancestor.
            if !has_merge_base(&local_base, &local_head) {
                return Err(CliError::Generic(format!(
                    "cannot find a common ancestor between {base} and {head}",
                )));
            }
            break;
        }
        last_count = count;
    }

    Ok((local_base, local_head))
}

/// Names of files changed between `base` and `head`. Uses the
/// `base...head` (three-dot) diff to compare `head` against
/// `merge-base(base, head)`, which is what CI scope detection
/// wants: "files this branch touched on top of trunk."
pub fn git_changed_files(base: &str, head: &str) -> Result<Vec<String>, CliError> {
    let (local_base, local_head) = ensure_history(base, head)?;
    let range = format!("{local_base}...{local_head}");
    // `--diff-filter=ACMRTD` matches Python: Added, Copied,
    // Modified, Renamed, Type-changed, Deleted. Excludes
    // Unmerged (U), Unknown (X), Broken (B).
    let out = run_git(&["diff", "--name-only", "--diff-filter=ACMRTD", &range, "--"])?;
    Ok(out
        .lines()
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_sha_matches_only_full_40char_lowercase_hex() {
        assert!(is_sha("0123456789abcdef0123456789abcdef01234567"));
        // Uppercase rejected (consistent with Python's regex
        // `^[0-9a-f]{40}$`).
        assert!(!is_sha("0123456789ABCDEF0123456789ABCDEF01234567"));
        // Shorter rejected — abbreviated SHAs would false-match
        // branch names like "deadbeef".
        assert!(!is_sha("deadbeef"));
        // Branch name (non-hex char).
        assert!(!is_sha("main"));
    }

    #[test]
    fn local_ref_namespacing() {
        // HEAD-relative refs pass through untouched.
        assert_eq!(local_ref("HEAD"), "HEAD");
        assert_eq!(local_ref("HEAD~1"), "HEAD~1");
        assert_eq!(local_ref("HEAD^2"), "HEAD^2");
        // Full SHAs pass through untouched.
        let sha = "a".repeat(40);
        assert_eq!(local_ref(&sha), sha);
        // Branch names get namespaced into our fetched prefix.
        assert_eq!(local_ref("main"), format!("{FETCHED_REF_PREFIX}main"));
    }

    #[test]
    fn fetch_arg_chooses_refspec_for_branches() {
        // No fetch needed for HEAD-relative refs.
        assert_eq!(fetch_arg("HEAD"), None);
        assert_eq!(fetch_arg("HEAD~3"), None);
        // SHAs get fetched by SHA directly.
        let sha = "b".repeat(40);
        assert_eq!(fetch_arg(&sha), Some(sha.clone()));
        // Branch names use a refspec so the result lands at a
        // local ref name we can target later in `git diff`.
        assert_eq!(
            fetch_arg("main"),
            Some(format!("+main:{FETCHED_REF_PREFIX}main")),
        );
    }
}
