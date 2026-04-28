//! Native Rust implementation of the `mergify config` subcommands.
//!
//! Phase 1.3 ports `config validate`. `config simulate` stays in the
//! Python shim until Phase 1.3b.

pub mod validate;
