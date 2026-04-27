//! Typed exit codes for the mergify CLI.
//!
//! Mirrors `mergify_cli.exit_codes.ExitCode` in the Python
//! implementation. The contract — which (command, failure mode)
//! maps to which exit code — is locked by Phase 0.1 and enforced by
//! the compat-test harness. Changing a variant's numeric value is a
//! breaking change for downstream scripts.

use std::process::ExitCode as ProcessExitCode;

/// Structured exit codes. Code 2 is reserved for Click's built-in
/// usage errors in the Python implementation and is therefore not
/// a variant here — it can only be produced by the CLI argument
/// parser (clap in Rust, click in Python).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ExitCode {
    Success = 0,
    GenericError = 1,
    StackNotFound = 3,
    Conflict = 4,
    GitHubApiError = 5,
    MergifyApiError = 6,
    InvalidState = 7,
    ConfigurationError = 8,
}

impl ExitCode {
    /// Raw u8 value suitable for `std::process::exit` or
    /// `std::process::ExitCode::from`.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

impl From<ExitCode> for u8 {
    fn from(code: ExitCode) -> Self {
        code.as_u8()
    }
}

impl From<ExitCode> for ProcessExitCode {
    fn from(code: ExitCode) -> Self {
        Self::from(code.as_u8())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_match_python_contract() {
        // These numeric values are the contract. Changing any of
        // them is a breaking change for downstream scripts.
        assert_eq!(ExitCode::Success.as_u8(), 0);
        assert_eq!(ExitCode::GenericError.as_u8(), 1);
        assert_eq!(ExitCode::StackNotFound.as_u8(), 3);
        assert_eq!(ExitCode::Conflict.as_u8(), 4);
        assert_eq!(ExitCode::GitHubApiError.as_u8(), 5);
        assert_eq!(ExitCode::MergifyApiError.as_u8(), 6);
        assert_eq!(ExitCode::InvalidState.as_u8(), 7);
        assert_eq!(ExitCode::ConfigurationError.as_u8(), 8);
    }

    #[test]
    fn two_is_not_used() {
        // Code 2 is reserved for Click/clap CLI argument errors.
        // No variant may shadow it.
        for code in [
            ExitCode::Success,
            ExitCode::GenericError,
            ExitCode::StackNotFound,
            ExitCode::Conflict,
            ExitCode::GitHubApiError,
            ExitCode::MergifyApiError,
            ExitCode::InvalidState,
            ExitCode::ConfigurationError,
        ] {
            assert_ne!(code.as_u8(), 2, "{code:?} must not use code 2");
        }
    }

    #[test]
    fn converts_to_u8() {
        let code: u8 = ExitCode::ConfigurationError.into();
        assert_eq!(code, 8);
    }

    #[test]
    fn converts_to_process_exit_code() {
        // ProcessExitCode is opaque, so we can't assert its numeric
        // value directly, but we verify the conversion at least
        // type-checks and does not panic.
        let _: ProcessExitCode = ExitCode::Success.into();
    }
}
