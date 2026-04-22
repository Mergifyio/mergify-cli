//! Shared foundations for the mergify CLI Rust port.
//!
//! This crate is intentionally empty in Phase 1.0 — it is the container
//! that subsequent phases fill with the HTTP client, auth, error types,
//! output traits, git operations, config loading, and interactive prompts.

/// Compile-time version string taken from the crate package metadata
/// via ``CARGO_PKG_VERSION``.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
