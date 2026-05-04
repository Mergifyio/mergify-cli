//! Native Rust implementation of the `mergify ci` subcommands.
//!
//! Phase 1.4 starts with `ci scopes-send` — straight HTTP POST to
//! Mergify with the scopes detected for a pull request. Other ci
//! commands (`git-refs`, `scopes`, `queue-info`, `junit-process`)
//! land in follow-up PRs as the shared infrastructure they need
//! (git-subprocess runner, GitHub event parser, `JUnit` XML reader)
//! is built out.

pub mod scopes_send;
