//! Native Rust implementation of the `mergify ci` subcommands.
//!
//! `ci scopes-send` was the first ported command. This module
//! adds `ci git-refs` (base/head detection) and the two shared
//! helpers it depends on: `github_event` (GitHub Actions event
//! payload deserialization) and `queue_metadata` (MQ YAML
//! fenced-block extraction). Subsequent ports (`queue-info`,
//! `junit-process`, `scopes` outer command) reuse these helpers.

pub mod detector;
pub mod git_refs;
pub mod github_event;
pub mod queue_info;
pub mod queue_metadata;
pub mod scopes_detect;
pub mod scopes_send;
pub mod tests_quarantine;
pub mod tests_show;

#[cfg(test)]
mod testing;
