//! `mergify` binary entry point.
//!
//! This is the Phase 1.0 scaffolding stub: it prints an identification
//! line and exits 0 so CI can verify the Rust toolchain, build, and
//! release profile end-to-end. Subsequent phases replace this with the
//! real clap-based dispatch that forwards commands to native Rust
//! implementations or the embedded Python shim.

fn main() {
    println!(
        "mergify {} — Rust scaffolding (Phase 1.0). \
         Use the Python CLI for actual functionality.",
        mergify_core::VERSION,
    );
}
