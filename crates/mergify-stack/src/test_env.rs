//! Test-only helper to isolate spawned `git` children from the
//! caller's `~/.gitconfig` and system config.
//!
//! Without this, parallel tests can race against each other when
//! the user's global git config carries hooks, includes, or
//! template directories that mutate per-invocation state. Symptom
//! is sporadic `git <foo> failed` panics in otherwise-pure tests
//! that just happen to spawn git as a side effect.
//!
//! Nothing here mutates the process environment — `mergify_core::env`
//! says why, and the rest of the workspace is being moved onto the
//! same footing — so this cannot be a `set_var` at process start. Instead [`isolated_git`] returns a fresh `Command`
//! with `GIT_CONFIG_GLOBAL=/dev/null` and `GIT_CONFIG_NOSYSTEM=1`
//! already on it. `Command::env` sets the *child's* environment, so
//! each git invocation carries the isolation itself; nothing is
//! shared and nothing has to be restored.
//!
//! It only covers the git commands that go through it. A `git` child
//! spawned by production code under test builds its own environment
//! from ours and sees neither these variables nor a test overlay, so
//! a fixture that needs isolation must create its repository state
//! through this helper.
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
