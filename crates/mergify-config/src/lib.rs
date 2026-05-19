//! Native Rust implementation of the `mergify config` subcommands.
//!
//! Hosts `config validate` and `config simulate`; both share the
//! config-file resolver in [`paths`].

pub mod paths;
pub mod simulate;
pub mod validate;
