//! Top-level CLI error type.
//!
//! Commands produce typed errors that map deterministically to an
//! [`ExitCode`]. The binary's `main()` converts a `CliError` into
//! the appropriate `ExitCode` and writes a human-readable message
//! to stderr before exiting.
//!
//! This enum grows as new error sources are added. Today it covers
//! the categories needed to port the `config` pilot (Phase 1.3);
//! subsequent sub-phases add variants for HTTP failures, git
//! subprocess failures, and so on.

use std::io;

use crate::exit_code::ExitCode;

#[derive(thiserror::Error, Debug)]
pub enum CliError {
    /// Configuration file missing, unparseable, or failing schema
    /// validation. Maps to [`ExitCode::ConfigurationError`].
    #[error("{0}")]
    Configuration(String),

    /// CLI invariant violated (e.g. branch targets itself, ambiguous
    /// commit reference, command run outside a merge queue context).
    /// Maps to [`ExitCode::InvalidState`].
    #[error("{0}")]
    InvalidState(String),

    /// Stack, branch, or commit not found. Maps to
    /// [`ExitCode::StackNotFound`].
    #[error("{0}")]
    StackNotFound(String),

    /// Rebase or merge conflict. Maps to [`ExitCode::Conflict`].
    #[error("{0}")]
    Conflict(String),

    /// Unclassified runtime failure (I/O error, bug, third-party
    /// panic captured and rethrown). Maps to
    /// [`ExitCode::GenericError`].
    #[error("{0}")]
    Generic(String),

    /// `std::io` error surfaced verbatim. Maps to
    /// [`ExitCode::GenericError`].
    #[error("{0}")]
    Io(#[from] io::Error),
}

impl CliError {
    /// The exit code the binary should return for this error.
    #[must_use]
    pub const fn exit_code(&self) -> ExitCode {
        match self {
            Self::Configuration(_) => ExitCode::ConfigurationError,
            Self::InvalidState(_) => ExitCode::InvalidState,
            Self::StackNotFound(_) => ExitCode::StackNotFound,
            Self::Conflict(_) => ExitCode::Conflict,
            Self::Generic(_) | Self::Io(_) => ExitCode::GenericError,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_mapping_is_total_and_stable() {
        assert_eq!(
            CliError::Configuration("x".into()).exit_code(),
            ExitCode::ConfigurationError,
        );
        assert_eq!(
            CliError::InvalidState("x".into()).exit_code(),
            ExitCode::InvalidState,
        );
        assert_eq!(
            CliError::StackNotFound("x".into()).exit_code(),
            ExitCode::StackNotFound,
        );
        assert_eq!(
            CliError::Conflict("x".into()).exit_code(),
            ExitCode::Conflict
        );
        assert_eq!(
            CliError::Generic("x".into()).exit_code(),
            ExitCode::GenericError
        );
        let io_err = io::Error::other("boom");
        assert_eq!(CliError::from(io_err).exit_code(), ExitCode::GenericError);
    }
}
