//! Append step outputs to `$GITHUB_OUTPUT`.
//!
//! A GitHub Actions step publishes its outputs by appending them to
//! the file named by `$GITHUB_OUTPUT`. The obvious `name=value` line
//! has no escape: a newline inside `value` ends the assignment, and
//! every following line is parsed as another output — so whatever
//! chose the value also chooses how many outputs the step declares.
//! The runner's own answer is the heredoc form
//!
//! ```text
//! name<<ghadelimiter_<32 hex>
//! value
//! ghadelimiter_<32 hex>
//! ```
//!
//! which is what [`append`] always emits. The delimiter is drawn from
//! the OS RNG per output, so a value cannot contain it and no call
//! site has to reason about what its value may hold. GitHub parses
//! both forms into the same output value, so consuming workflows see
//! no difference.
//!
//! One writer in the crate does not go through here:
//! `junit_process::command::maybe_write_github_output` writes
//! `test_results_upload=<status>` in the bare form. Its value is one
//! of three `&'static str` literals, and it is deliberately
//! best-effort ("reporting plumbing must never break the run") where
//! [`append`] propagates a `CliError`. Make that status dynamic and
//! it has to move here first.
//!
//! The heredoc protects the value half of the pair; the name half is
//! protected by its type. `&'static str` cannot hold a name derived
//! from a payload, and a name is the other place a newline (or an `=`
//! ahead of the `<<`) would let the runner read the block as
//! something else.

use std::env;
use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::PathBuf;

use mergify_core::CliError;

/// Append each `(name, value)` pair to `$GITHUB_OUTPUT` as a step
/// output. No-op when the variable is unset or empty — i.e. anywhere
/// but a GitHub Actions runner.
pub(crate) fn append(outputs: &[(&'static str, &str)]) -> Result<(), CliError> {
    let Some(path) = env::var("GITHUB_OUTPUT").ok().filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    // Assembled first, then written once. Three `writeln!` calls on an
    // unbuffered `File` are several `write_all` syscalls each, and a
    // sequence cut in the middle leaves an unterminated heredoc, which
    // fails the step outright ("Matching delimiter not found") — a
    // worse outcome than the truncated line the `name=value` form
    // would have left.
    let mut block = String::new();
    for (name, value) in outputs {
        let delimiter = format!("ghadelimiter_{}", random_delimiter_suffix()?);
        // Writing to a String is infallible.
        let _ = writeln!(block, "{name}<<{delimiter}\n{value}\n{delimiter}");
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(PathBuf::from(path))?
        .write_all(block.as_bytes())?;
    Ok(())
}

/// 16 random bytes rendered as 32 lowercase hex chars — enough
/// entropy to be unguessable inside one GitHub Actions step, which is
/// all the heredoc delimiter needs (it just has to be absent from the
/// value). `getrandom` reads the OS RNG directly; we don't need the
/// parsing/formatting plumbing `uuid` adds on top.
fn random_delimiter_suffix() -> Result<String, CliError> {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf).map_err(|e| CliError::wrap("draw a $GITHUB_OUTPUT delimiter", e))?;
    let mut hex = String::with_capacity(buf.len() * 2);
    for b in buf {
        // Writing to a String is infallible.
        let _ = write!(hex, "{b:02x}");
    }
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_is_a_noop_outside_github_actions() {
        temp_env::with_var("GITHUB_OUTPUT", None::<&str>, || {
            append(&[("k", "v")]).unwrap();
        });
        // An empty value is treated the same as unset: the runner
        // exports `GITHUB_OUTPUT=` in some contexts, and an empty
        // path is not openable.
        temp_env::with_var("GITHUB_OUTPUT", Some(""), || {
            append(&[("k", "v")]).unwrap();
        });
    }

    #[test]
    fn append_wraps_every_output_in_its_own_heredoc() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gha_output");
        temp_env::with_var("GITHUB_OUTPUT", Some(path.to_str().unwrap()), || {
            append(&[("base", "cafef00d"), ("head", "0badc0de")]).unwrap();
        });
        let written = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines.len(), 6, "got: {written:?}");
        assert!(
            lines[0].starts_with("base<<ghadelimiter_"),
            "got: {written:?}"
        );
        assert_eq!(lines[1], "cafef00d");
        assert_eq!(lines[2], &lines[0]["base<<".len()..]);
        assert!(
            lines[3].starts_with("head<<ghadelimiter_"),
            "got: {written:?}"
        );
        assert_eq!(lines[4], "0badc0de");
        assert_eq!(lines[5], &lines[3]["head<<".len()..]);
        // Each output draws its own delimiter.
        assert_ne!(lines[2], lines[5]);
    }

    #[test]
    fn a_newline_in_a_value_cannot_declare_another_output() {
        // The whole point of the heredoc: the injected `evil=` line
        // sits inside `base`'s body rather than becoming a second
        // step output (MRGFY-8845).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gha_output");
        temp_env::with_var("GITHUB_OUTPUT", Some(path.to_str().unwrap()), || {
            append(&[("base", "cafef00d\nevil=1")]).unwrap();
        });
        let written = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = written.lines().collect();
        let delimiter = &lines[0]["base<<".len()..];
        assert_eq!(lines.len(), 4, "got: {written:?}");
        assert_eq!(&lines[1..3], ["cafef00d", "evil=1"]);
        assert_eq!(lines[3], delimiter);
    }

    #[test]
    fn append_keeps_earlier_content() {
        // The runner accumulates every step's outputs in one file, so
        // the write must append rather than truncate.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gha_output");
        std::fs::write(&path, "earlier=1\n").unwrap();
        temp_env::with_var("GITHUB_OUTPUT", Some(path.to_str().unwrap()), || {
            append(&[("base", "cafef00d")]).unwrap();
        });
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.starts_with("earlier=1\n"), "got: {written:?}");
    }
}
