//! Native Rust implementation of the `mergify queue` subcommands.
//!
//! Hosts `pause` / `unpause` (idempotent API mutations), `status`
//! (read-only batch tree + waiting list, with JSON passthrough),
//! and `show` (per-PR detail with checks + conditions tree, plus the
//! activity-log fallback — `mergify_events::queue_leave` — that tells
//! a dequeued PR apart from a never-queued one).

pub mod pause;
pub mod show;
pub mod status;
pub mod unpause;
