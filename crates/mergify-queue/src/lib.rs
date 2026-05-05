//! Native Rust implementation of the `mergify queue` subcommands.
//!
//! Phase 1.5 ported `pause` and `unpause` — two idempotent API
//! calls that rest on the HTTP client added in 1.2b and the
//! `put`/`delete_if_exists` methods added alongside this crate.
//! Phase 1.7 ports `status`, the read-only command that fetches
//! the merge-queue snapshot and renders it either as a JSON
//! passthrough or as the human-friendly batch tree + waiting list.
//! `queue show` stays shimmed until its conditions/checks tree
//! ports next.

pub mod auth;
pub mod pause;
pub mod status;
pub mod unpause;
