//! Shared foundations for the mergify CLI Rust port.
//!
//! - [`exit_code::ExitCode`] — typed exit codes mirroring the
//!   Python `exit_codes.py` contract.
//! - [`error::CliError`] — top-level error enum with deterministic
//!   mapping to an `ExitCode`.
//! - [`output::Output`] — trait for emitting command results in
//!   either JSON or human mode with stdout/stderr discipline baked
//!   in.
//! - [`http::Client`] — wraps `reqwest` with bearer auth, retry,
//!   and typed error mapping for the Mergify and GitHub APIs.
//! - [`auth`] — resolve `--repository` / `--token` / `--api-url`
//!   from the same flag → env → fallback chain the Python CLI uses.
//! - [`command_context::CommandContext`] — bundle the resolved
//!   trio + a pre-configured Mergify HTTP client for the
//!   queue/freeze command preludes.

pub mod auth;
pub mod command_context;
pub mod env;
pub mod error;
pub mod exit_code;
pub mod http;
pub mod output;
pub mod pull_request;

pub use command_context::CommandContext;
pub use error::CliError;
pub use exit_code::ExitCode;
pub use http::{ApiFlavor, Client as HttpClient, DeleteOutcome, RetryPolicy};
pub use output::{Output, OutputMode, StdioOutput};

/// Compile-time version string taken from the crate package metadata
/// via ``CARGO_PKG_VERSION``.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
