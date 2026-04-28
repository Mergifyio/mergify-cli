//! Native Rust implementation of the `mergify queue` subcommands.
//!
//! Phase 1.5 ports `pause` and `unpause` — two idempotent API
//! calls that rest on the HTTP client added in 1.2b and the new
//! `put`/`delete_if_exists` methods added alongside this crate.
//! `queue status` and `queue show` stay shimmed until their
//! JSON-output contracts are locked (they carry considerable
//! structured data and want careful schema work).

pub mod auth;
pub mod pause;
pub mod unpause;
