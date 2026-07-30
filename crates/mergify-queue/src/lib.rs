//! Native Rust implementation of the `mergify queue` subcommands.
//!
//! Hosts `pause` / `unpause` (idempotent API mutations), `status`
//! (read-only batch tree + waiting list, with JSON passthrough),
//! `show` (per-PR detail with checks + conditions tree), and
//! `history` (per-PR event trail, which unlike the other read
//! commands still answers after the PR left the queue).

pub mod history;
pub mod pause;
pub mod show;
pub mod status;
pub mod unpause;
