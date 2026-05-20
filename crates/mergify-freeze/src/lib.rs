//! Native Rust implementation of the `mergify freeze` subcommands.
//!
//! `freeze list` is the first port — a read-only `GET` on
//! `/v1/repos/<repo>/scheduled_freeze` with either a JSON
//! passthrough of the inner `scheduled_freezes` array or a
//! human-readable table. `create` / `update` / `delete` follow
//! the same module-per-subcommand layout once they land.

pub mod list;
