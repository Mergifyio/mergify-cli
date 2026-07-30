//! Native Rust implementation of the `mergify queue` subcommands.
//!
//! Hosts `pause` / `unpause` (idempotent API mutations), `status`
//! (read-only batch tree + waiting list, with JSON passthrough),
//! and `show` (per-PR detail with checks + conditions tree, plus the
//! [`last_leave`] activity-log fallback that tells a dequeued PR
//! apart from a never-queued one).

pub mod last_leave;
pub mod pause;
pub mod show;
pub mod status;
pub mod unpause;
