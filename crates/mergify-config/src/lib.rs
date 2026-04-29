//! Native Rust implementation of the `mergify config` subcommands.
//!
//! Phase 1.3 ports `config validate`. Phase 1.3b adds `config
//! simulate`. Both share the config-file resolver in [`paths`].

pub mod paths;
pub mod simulate;
pub mod validate;
