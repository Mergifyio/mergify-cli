//! Native Rust implementation of the `mergify ci` subcommands.
//!
//! Phase 1.4 landed `ci scopes-send`. Phase 1.6 adds `ci queue-info`
//! and `ci git-refs`, which share GitHub event parsing and MQ
//! metadata extraction (`github_event` + `queue_metadata`
//! modules). Remaining commands (`scopes`, `junit-process`,
//! `junit-upload`) follow once the shared infrastructure they need
//! is in place.

pub mod git_refs;
pub mod github_event;
pub mod queue_info;
pub mod queue_metadata;
pub mod scopes_send;
