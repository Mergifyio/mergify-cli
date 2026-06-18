//! Typed exit codes for the mergify CLI.
//!
//! Mirrors `mergify_cli.exit_codes.ExitCode` in the Python
//! implementation. The contract — which (command, failure mode)
//! maps to which exit code — is enforced by the compat-test
//! harness. Changing a variant's numeric value is a breaking change
//! for downstream scripts.

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
    /// Every variant, in numeric order. The single source of truth for
    /// enumerating exit codes — the published CLI schema reads this.
    pub const ALL: [Self; 8] = [
        Self::Success,
        Self::GenericError,
        Self::StackNotFound,
        Self::Conflict,
        Self::GitHubApiError,
        Self::MergifyApiError,
        Self::InvalidState,
        Self::ConfigurationError,
    ];

    /// Raw u8 value suitable for `std::process::exit` or
    /// `std::process::ExitCode::from`.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Stable identifier — the enum variant name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Success => "Success",
            Self::GenericError => "GenericError",
            Self::StackNotFound => "StackNotFound",
            Self::Conflict => "Conflict",
            Self::GitHubApiError => "GitHubApiError",
            Self::MergifyApiError => "MergifyApiError",
            Self::InvalidState => "InvalidState",
            Self::ConfigurationError => "ConfigurationError",
        }
    }

    /// One-line meaning for the published reference. Mirrors the
    /// `CliError` variant that produces each code.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Success => "Command completed successfully.",
            Self::GenericError => {
                "Unclassified runtime failure (I/O error, bug, or captured panic)."
            }
            Self::StackNotFound => "Stack, branch, or commit not found.",
            Self::Conflict => "Rebase or merge conflict.",
            Self::GitHubApiError => "GitHub API request failed.",
            Self::MergifyApiError => "Mergify API request failed.",
            Self::InvalidState => {
                "CLI invariant violated (e.g. command run outside a valid context)."
            }
            Self::ConfigurationError => {
                "Configuration file missing, unparseable, or failing validation."
            }
        }
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
    use std::collections::BTreeSet;

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
        for code in ExitCode::ALL {
            assert_ne!(code.as_u8(), 2, "{code:?} must not use code 2");
        }
    }

    #[test]
    fn all_is_complete_and_ordered() {
        // `ALL` is hand-maintained; a forgotten variant would silently
        // drop from the published schema. Pin its codes against the
        // contract, and require strictly ascending order so a stray
        // duplicate or reordering is caught here.
        let codes: Vec<u8> = ExitCode::ALL.iter().map(|c| c.as_u8()).collect();
        assert_eq!(codes, [0, 1, 3, 4, 5, 6, 7, 8]);
        assert!(
            codes.windows(2).all(|w| w[0] < w[1]),
            "ALL must be strictly ascending",
        );
    }

    #[test]
    fn names_and_descriptions_are_present_and_unique() {
        let names: BTreeSet<&str> = ExitCode::ALL.iter().map(|c| c.name()).collect();
        assert_eq!(names.len(), ExitCode::ALL.len(), "names must be unique");
        for code in ExitCode::ALL {
            assert!(!code.name().is_empty(), "{code:?} name is empty");
            assert!(
                !code.description().is_empty(),
                "{code:?} description is empty",
            );
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
