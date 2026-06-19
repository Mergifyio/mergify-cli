//! Top-level CLI error type.
//!
//! Commands produce typed errors that map deterministically to an
//! [`ExitCode`]. The binary's `main()` converts a `CliError` into
//! the appropriate `ExitCode`, writes a human-readable message to
//! stderr, and walks the [`std::error::Error::source`] chain so any
//! preserved cause prints as a `caused by:` line before exiting.
//!
//! This enum grows as new error sources are added — add a variant
//! per error category, never a generic `String` catch-all for new
//! kinds of failure that have their own exit code.
//!
//! Prefer preserving a cause over flattening it into a string:
//! - a self-describing typed error → [`CliError::Source`] (or `?` it
//!   through the generated `From`), which keeps it transparently;
//! - "doing X failed because of Y" → [`CliError::wrap`], which shows
//!   the context as the headline and Y as a `caused by:` line.
//!
//! Reach for `CliError::Generic(e.to_string())` only when there is
//! genuinely no typed cause worth keeping.

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

    /// GitHub API failure (HTTP error against github.com). Maps to
    /// [`ExitCode::GitHubApiError`].
    #[error("{0}")]
    GitHubApi(String),

    /// Mergify API failure (HTTP error against the Mergify
    /// service). Maps to [`ExitCode::MergifyApiError`].
    #[error("{0}")]
    MergifyApi(String),

    /// Unclassified runtime failure (I/O error, bug, third-party
    /// panic captured and rethrown). Maps to
    /// [`ExitCode::GenericError`].
    #[error("{0}")]
    Generic(String),

    /// `std::io` error surfaced verbatim. Its `Display` is the whole
    /// message, so it is intentionally not exposed as a chainable
    /// source (that would print the same text twice). Maps to
    /// [`ExitCode::GenericError`].
    #[error("{0}")]
    Io(io::Error),

    /// A typed lower-level error preserved verbatim and transparently
    /// (same `Display`, same source chain), so it survives as a
    /// downcastable cause instead of being flattened to a string.
    /// Maps to [`ExitCode::GenericError`].
    #[error(transparent)]
    Source(#[from] Box<dyn std::error::Error + Send + Sync + 'static>),

    /// A contextual headline plus a preserved underlying cause. The
    /// `context` is the message; `source` prints as a `caused by:`
    /// line. Build with [`CliError::wrap`]. Maps to
    /// [`ExitCode::GenericError`].
    #[error("{context}")]
    Wrapped {
        context: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
}

impl CliError {
    /// Wrap a lower-level error with a contextual message, keeping
    /// the original as a `caused by:` source for the printed chain.
    pub fn wrap(
        context: impl Into<String>,
        source: impl Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
    ) -> Self {
        Self::Wrapped {
            context: context.into(),
            source: source.into(),
        }
    }

    /// The exit code the binary should return for this error.
    #[must_use]
    pub const fn exit_code(&self) -> ExitCode {
        match self {
            Self::Configuration(_) => ExitCode::ConfigurationError,
            Self::InvalidState(_) => ExitCode::InvalidState,
            Self::StackNotFound(_) => ExitCode::StackNotFound,
            Self::Conflict(_) => ExitCode::Conflict,
            Self::GitHubApi(_) => ExitCode::GitHubApiError,
            Self::MergifyApi(_) => ExitCode::MergifyApiError,
            Self::Generic(_) | Self::Io(_) | Self::Source(_) | Self::Wrapped { .. } => {
                ExitCode::GenericError
            }
        }
    }
}

impl From<io::Error> for CliError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
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
            CliError::GitHubApi("x".into()).exit_code(),
            ExitCode::GitHubApiError,
        );
        assert_eq!(
            CliError::MergifyApi("x".into()).exit_code(),
            ExitCode::MergifyApiError,
        );
        assert_eq!(
            CliError::Generic("x".into()).exit_code(),
            ExitCode::GenericError
        );
        let io_err = io::Error::other("boom");
        assert_eq!(CliError::from(io_err).exit_code(), ExitCode::GenericError);
        let boxed: Box<dyn std::error::Error + Send + Sync> = "boom".into();
        assert_eq!(CliError::from(boxed).exit_code(), ExitCode::GenericError);
        assert_eq!(
            CliError::wrap("x", io::Error::other("y")).exit_code(),
            ExitCode::GenericError,
        );
    }

    #[test]
    fn io_display_is_not_also_a_source() {
        // `Io`'s Display is the whole message; exposing it as a
        // source too would double-print it in the `caused by:` chain.
        let err = CliError::from(io::Error::other("disk gone"));
        assert_eq!(err.to_string(), "disk gone");
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn wrapped_keeps_context_as_headline_and_cause_as_source() {
        let err = CliError::wrap("write config", io::Error::other("disk gone"));
        assert_eq!(err.to_string(), "write config");
        let cause = std::error::Error::source(&err).expect("cause preserved");
        assert_eq!(cause.to_string(), "disk gone");
    }

    #[test]
    fn source_is_transparent_for_a_self_describing_leaf() {
        // A transparent `Source` shows the inner Display and delegates
        // the chain to the inner error (a leaf has no further cause).
        let err: CliError = CliError::Source(Box::new(io::Error::other("boom")));
        assert_eq!(err.to_string(), "boom");
        assert!(std::error::Error::source(&err).is_none());
    }
}
