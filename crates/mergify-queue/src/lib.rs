//! Native Rust implementation of the `mergify queue` subcommands.
//!
//! Hosts `pause` / `unpause` (idempotent API mutations), `status`
//! (read-only batch tree + waiting list, with JSON passthrough),
//! and `show` (per-PR detail with checks + conditions tree).

pub mod pause;
pub mod show;
pub mod status;
pub mod unpause;
