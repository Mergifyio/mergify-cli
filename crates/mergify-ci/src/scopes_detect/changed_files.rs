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

use std::path::Path;
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

/// Base `git` command, rooted at `repo_dir` via `-C` when one is
/// given. Production always passes `None` (the process cwd); the
/// parameter is what lets the tests below drive a fixture
/// repository without `std::env::set_current_dir`, which races
/// with parallel cargo test workers. Same shape as
/// `mergify_stack::git::git_cmd`, C locale included — `run_git`
/// quotes git's stderr into its error, and a translated message
/// would leave a French sentence inside an English CLI error.
fn git_cmd(repo_dir: Option<&Path>) -> Command {
    let mut cmd = Command::new("git");
    if let Some(dir) = repo_dir {
        cmd.arg("-C").arg(dir);
    }
    cmd.env("LC_ALL", "C").env("LANG", "C").env("LANGUAGE", "C");
    cmd
}

fn run_git(repo_dir: Option<&Path>, args: &[&str]) -> Result<String, CliError> {
    let out = git_cmd(repo_dir)
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
    // Untrimmed: `git_changed_files` reads NUL-delimited paths,
    // and a leading or trailing space is a legal filename
    // character. Callers that parse a scalar trim it themselves.
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn has_merge_base(repo_dir: Option<&Path>, base: &str, head: &str) -> bool {
    git_cmd(repo_dir)
        .args(["merge-base", "--", base, head])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn commits_count(repo_dir: Option<&Path>) -> Result<u64, CliError> {
    let out = run_git(repo_dir, &["rev-list", "--count", "--all"])?;
    let count = out.trim();
    count
        .parse::<u64>()
        .map_err(|e| CliError::Generic(format!("could not parse commit count {count:?}: {e}")))
}

fn fetch(repo_dir: Option<&Path>, depth_flag: &str, fetch_args: &[String]) -> Result<(), CliError> {
    let mut args: Vec<&str> = vec!["fetch", "--no-tags", depth_flag, "origin"];
    if !fetch_args.is_empty() {
        args.push("--");
        for fa in fetch_args {
            args.push(fa);
        }
    }
    run_git(repo_dir, &args).map(drop)
}

/// Deepen the local clone until `base` and `head` share an
/// ancestor (or until we've exhausted history). Returns the
/// pair of local ref names (`refs/mergify-cli/fetched/<ref>` for
/// remote names; `HEAD~N` / `HEAD^N` / SHA passed through
/// untouched) that the subsequent `git diff` should target.
pub fn ensure_history(
    repo_dir: Option<&Path>,
    base: &str,
    head: &str,
) -> Result<(String, String), CliError> {
    if has_merge_base(repo_dir, base, head) {
        return Ok((base.to_string(), head.to_string()));
    }

    let fetch_args: Vec<String> = [fetch_arg(base), fetch_arg(head)]
        .into_iter()
        .flatten()
        .collect();
    let local_base = local_ref(base);
    let local_head = local_ref(head);
    let mut depth = COMMITS_BATCH_SIZE;

    fetch(repo_dir, &format!("--depth={depth}"), &fetch_args)?;

    let mut last_count = commits_count(repo_dir)?;
    while !has_merge_base(repo_dir, &local_base, &local_head) {
        depth = depth.saturating_mul(2);
        fetch(repo_dir, &format!("--deepen={depth}"), &fetch_args)?;
        let count = commits_count(repo_dir)?;
        if count == last_count {
            // No new commits this round — we've reached the root
            // and the refs genuinely have no common ancestor.
            if !has_merge_base(repo_dir, &local_base, &local_head) {
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
///
/// A rename yields **both** of its paths, so a scope the file moved
/// out of still counts as touched. Paths are not
/// `core.quotePath`-escaped, so the globs see the real name; bytes
/// that aren't valid UTF-8 are still replaced (see `run_git`), and
/// the result is unescaped, so callers that print a path must
/// escape it first.
pub fn git_changed_files(
    repo_dir: Option<&Path>,
    base: &str,
    head: &str,
) -> Result<Vec<String>, CliError> {
    let (local_base, local_head) = ensure_history(repo_dir, base, head)?;
    let range = format!("{local_base}...{local_head}");
    // `--no-renames` is what gives a rename both of its paths: git
    // emits `D <source>` + `A <destination>` rather than pairing
    // them into one `R` entry, whose `--name-only` line carries the
    // destination alone. The engine treats a rename the same way,
    // added-at-destination and removed-at-source (MRGFY-8248); the
    // two must agree or CLI-uploaded and engine-derived scopes
    // disagree for the same pull request.
    //
    // `--diff-filter=ACMRTD` matches Python: Added, Modified,
    // Type-changed, Deleted, plus the Renamed and Copied letters
    // that `--no-renames` has just made unreachable. They stay so
    // that removing `--no-renames` degrades to the old
    // destination-only behavior rather than to reporting nothing.
    // Excludes Unmerged (U), Unknown (X), Broken (B).
    //
    // `diff.relative` is forced off: set in a repo's config, it
    // reports paths relative to the process cwd, so running
    // `mergify ci scopes` from a subdirectory would hand the
    // globs `r.md` for `docs/r.md` and drop every path outside
    // that subtree. Scope globs are anchored to the repo root, so
    // nothing would match and every scope-gated job would be
    // skipped. Overridden through `-c` rather than
    // `--no-relative`, which only exists since git 2.28.
    //
    // `-z` suppresses the `core.quotePath` escaping git otherwise
    // applies to any path holding a non-ASCII byte, a quote, or a
    // backslash — line-based output hands the scope globs
    // `"critical/cach\303\251.txt"`, quotes included, and nothing
    // matches it (MRGFY-8287). Setting `core.quotePath=false`
    // would not do: git escapes `"` and `\` whatever that says.
    // `-z` also removes the only reason a path could not contain
    // a newline, which is why the printers in `super` escape
    // before echoing one.
    let out = run_git(
        repo_dir,
        &[
            "-c",
            "diff.relative=false",
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            "--diff-filter=ACMRTD",
            &range,
            "--",
        ],
    )?;
    Ok(out
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(ToString::to_string)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run `git` in `dir` with the developer's global/system
    /// config out of the way, so a personal `diff.renames` or
    /// `core.quotePath` setting can't change what these tests see.
    ///
    /// `GIT_DIR`/`GIT_WORK_TREE` are dropped too: they override
    /// `-C`, and this helper is the one that runs `add`/`commit`.
    /// Under `git bisect run cargo test` (or a hook, or `git
    /// rebase -x`) those are set, and the fixture's `git add -A`
    /// would otherwise stage into the developer's real repository.
    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .args(args)
            .status()
            .expect("spawn git")
            .success();
        assert!(ok, "git {args:?} failed");
    }

    fn write(dir: &Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().expect("path has a parent")).expect("mkdir");
        std::fs::write(path, content).expect("write");
    }

    /// A repo on `main` holding `critical/` and `docs/readme.md`,
    /// checked out on `feature` so the caller can commit its change
    /// there. `ignored/` is made in the working tree afterwards and
    /// stays untracked — git has no empty directories, and the
    /// `git mv` case needs its destination to exist.
    ///
    /// `diff.renames` and `core.quotePath` are pinned to git's
    /// defaults in the repo-local config, which beats whatever the
    /// developer or CI runner has globally: each test below guards
    /// a behavior that only misfires when the corresponding
    /// default is in force.
    fn fixture() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@e.com"],
            &["config", "user.name", "T"],
            &["config", "diff.renames", "true"],
            &["config", "core.quotePath", "true"],
        ] {
            git(dir, args);
        }
        write(dir, "critical/guard.txt", "guard\n");
        write(dir, "critical/caché.txt", "accented\n");
        write(dir, "critical/quote\"and\\slash.txt", "punctuation\n");
        write(dir, "docs/readme.md", "docs\n");
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "base"]);
        git(dir, &["checkout", "-q", "-b", "feature"]);
        std::fs::create_dir(dir.join("ignored")).expect("mkdir ignored");
        tmp
    }

    /// Sorted, because `git_changed_files` promises a set of paths
    /// and not an order — git's own ordering is steered by
    /// `diff.orderFile`, which the production call inherits from
    /// the developer's global config.
    fn changed(dir: &Path) -> Vec<String> {
        let mut paths = git_changed_files(Some(dir), "main", "HEAD").expect("git diff");
        paths.sort();
        paths
    }

    #[test]
    fn rename_yields_source_and_destination() {
        // MRGFY-8286: with rename detection on, `--name-only`
        // collapsed a `git mv` to the destination alone, so the
        // scope the file moved *out of* was never reported and a
        // scope-conditioned CI job for it got skipped — even
        // though the resulting `critical/` tree is identical to
        // the one a plain deletion produces.
        let tmp = fixture();
        let dir = tmp.path();
        git(dir, &["mv", "critical/guard.txt", "ignored/guard.txt"]);
        git(dir, &["commit", "-q", "-m", "move the guard out"]);

        assert_eq!(changed(dir), ["critical/guard.txt", "ignored/guard.txt"]);
    }

    #[test]
    fn non_ascii_path_is_not_quote_escaped() {
        // MRGFY-8287: `core.quotePath` is on by default, so
        // line-based output rendered this path as
        // `"critical/cach\303\251.txt"` — quotes and octal escapes
        // included — which matches no scope glob, leaving repos
        // with accented or CJK filenames detecting nothing.
        let tmp = fixture();
        let dir = tmp.path();
        write(dir, "critical/caché.txt", "accented, changed\n");
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "touch the accented file"]);

        assert_eq!(changed(dir), ["critical/caché.txt"]);
    }

    #[test]
    fn quote_and_backslash_path_is_not_escaped() {
        // The half of MRGFY-8287 that `core.quotePath=false` does
        // *not* cover: git escapes `"` and `\` in a path whatever
        // that setting says, so only `-z` gets these through. A
        // fix that reached for the config instead would pass
        // `non_ascii_path_is_not_quote_escaped` and still leave
        // this repo silently unscoped.
        let tmp = fixture();
        let dir = tmp.path();
        write(
            dir,
            "critical/quote\"and\\slash.txt",
            "punctuation, changed\n",
        );
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "touch the punctuated file"]);

        assert_eq!(changed(dir), ["critical/quote\"and\\slash.txt"]);
    }

    #[test]
    fn add_modify_and_delete_are_unaffected() {
        // Every ordinary status still reaches the caller. This
        // does not pin `--no-renames` — the deleted and added
        // files share no content, so git would never pair them
        // even with rename detection on; `rename_yields_source_
        // and_destination` is the test that holds the flag down.
        let tmp = fixture();
        let dir = tmp.path();
        write(dir, "ignored/new.txt", "new\n");
        write(dir, "docs/readme.md", "docs changed\n");
        std::fs::remove_file(dir.join("critical/guard.txt")).expect("rm");
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "add, modify, delete"]);

        assert_eq!(
            changed(dir),
            ["critical/guard.txt", "docs/readme.md", "ignored/new.txt",],
        );
    }

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
