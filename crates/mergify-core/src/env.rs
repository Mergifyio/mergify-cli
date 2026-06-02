//! Environment-variable helpers shared across commands.
//!
//! The CLI's resolver pattern (`flag → env → default`) treats the
//! empty string as "not set", because callers in the wild — most
//! notably the `gha-mergify-ci` GitHub Action — `export VAR=""`
//! when no value is available. Inlining
//! `std::env::var(NAME).ok().filter(|s| !s.is_empty())` on every call
//! site looks innocuous but invites the same bug we've now hit
//! twice (monorepo#33423, `MERGIFY_CONFIG_PATH` and
//! `MERGIFY_TEST_EXIT_CODE`): a contributor adds clap's
//! `env = "MERGIFY_FOO"` attribute on a flag instead, and clap's
//! parser treats an empty env value as a present-but-empty flag
//! value, aborting parsing before any of our code can fall back.
//!
//! Use [`var_non_empty`] for the env-var leg of any `flag → env
//! → default` chain. Do **not** wire env vars through clap's
//! `env = ...` attribute for any of the `MERGIFY_*` namespace.

use std::env;

/// Read an environment variable and return its value if it's set
/// to a non-empty string. Unset or empty both collapse to `None`.
///
/// This is the standard primitive for the env-var leg of the
/// `--flag → env → default` resolver chain across every ported
/// command. See the module doc for the empty-as-unset rationale.
#[must_use]
pub fn var_non_empty(name: &str) -> Option<String> {
    env::var(name).ok().filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_some_for_non_empty_value() {
        let got = temp_env::with_var("MERGIFY_TEST_HELPER_X", Some("hello"), || {
            var_non_empty("MERGIFY_TEST_HELPER_X")
        });
        assert_eq!(got.as_deref(), Some("hello"));
    }

    #[test]
    fn returns_none_for_empty_value() {
        // The whole point of this helper: an empty env var is
        // treated as if it were not set. Regression-prone enough
        // that we pin it explicitly.
        let got = temp_env::with_var("MERGIFY_TEST_HELPER_Y", Some(""), || {
            var_non_empty("MERGIFY_TEST_HELPER_Y")
        });
        assert_eq!(got, None);
    }

    #[test]
    fn returns_none_when_unset() {
        let got = temp_env::with_var_unset("MERGIFY_TEST_HELPER_Z", || {
            var_non_empty("MERGIFY_TEST_HELPER_Z")
        });
        assert_eq!(got, None);
    }
}
