//! Shared foundations for the mergify CLI Rust port.
//!
//! Phase 1.2 populates this crate with:
//!
//! - [`exit_code::ExitCode`] — typed exit codes mirroring the
//!   Python `exit_codes.py` contract.
//! - [`error::CliError`] — top-level error enum with deterministic
//!   mapping to an `ExitCode`.
//! - [`output::Output`] — trait for emitting command results in
//!   either JSON or human mode with stdout/stderr discipline baked
//!   in.
//!
//! HTTP client, git operations, interactive prompts, and config
//! loading arrive in subsequent sub-phases.

pub mod error;
pub mod exit_code;
pub mod output;

pub use error::CliError;
pub use exit_code::ExitCode;
pub use output::{Output, OutputMode, StdioOutput};

/// Compile-time version string taken from the crate package metadata
/// via ``CARGO_PKG_VERSION``.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
