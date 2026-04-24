//! `mergify` binary entry point.
//!
//! In Phase 1.1 the binary is purely a wrapper: every command is
//! handed off to [`mergify_py_shim::run`], which extracts the
//! embedded Python source on first use and invokes
//! `python3 -m mergify_cli`. From an external observer's view,
//! running the Rust binary is indistinguishable from running the
//! Python CLI directly.
//!
//! Phase 1.3+ will start intercepting specific subcommands (starting
//! with `config validate`) and dispatch them to native Rust
//! implementations, falling back to [`mergify_py_shim::run`] only
//! for the commands that haven't been ported yet.

use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match mergify_py_shim::run(&args) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(err) => {
            eprintln!("mergify: {err}");
            ExitCode::FAILURE
        }
    }
}
