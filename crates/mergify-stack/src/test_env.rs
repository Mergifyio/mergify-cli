//! Test-only helper to isolate spawned `git` children from the
//! caller's `~/.gitconfig` and system config.
//!
//! Without this, parallel tests can race against each other when
//! the user's global git config carries hooks, includes, or
//! template directories that mutate per-invocation state. Symptom
//! is sporadic `git <foo> failed` panics in otherwise-pure tests
//! that just happen to spawn git as a side effect.
//!
//! The workspace forbids `unsafe_code`, so we can't `set_var` at
//! process start. Instead, [`isolated_git`] returns a fresh
//! `Command` with `GIT_CONFIG_GLOBAL=/dev/null` and
//! `GIT_CONFIG_NOSYSTEM=1` pre-applied; child git invocations
//! made *by the production code under test* will inherit these
//! when the parent test set them via the same helper before any
//! production call — i.e. wire `isolated_git` through the test
//! fixtures that build the repository, and the production code's
//! own `git` children pick up the same env via inheritance from
//! the spawned-fixture parent process (us).
//!
//! Practically: call [`isolated_git`] wherever the tests used to
//! call `std::process::Command::new("git")`.

use std::process::Command;

/// Build a `git` command with both `GIT_CONFIG_GLOBAL` and
/// `GIT_CONFIG_NOSYSTEM` set so it ignores the caller's user and
/// system git configuration.
pub fn isolated_git() -> Command {
    let mut cmd = Command::new("git");
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
    cmd.env("GIT_CONFIG_NOSYSTEM", "1");
    cmd
}
