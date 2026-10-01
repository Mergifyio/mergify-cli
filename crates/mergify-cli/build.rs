//! Plumb the release version through to the binary.
//!
//! The release tag (calver, `2026.10.1`) is the source of truth
//! for the version, so `Cargo.toml` is pinned at the `0.0.0`
//! placeholder and the real version comes in via the
//! `MERGIFY_RELEASE_VERSION` env var the release workflow sets
//! from `$GITHUB_REF`. This script normalises that input into a
//! single rustc-env (`MERGIFY_CLI_VERSION`) the binary reads
//! unconditionally — empty / unset both collapse to
//! `CARGO_PKG_VERSION` here, so `main.rs` is a one-liner that
//! can't accidentally surface an empty version string.

fn main() {
    // Rebuild when the env var changes so a release rebuild after a
    // dev build actually picks up the new value.
    println!("cargo:rerun-if-env-changed=MERGIFY_RELEASE_VERSION");
    // A build script's environment is cargo's, handed to it for this
    // one invocation — not the process environment `clippy.toml`
    // guards, and not reachable through `mergify_core::env`, which a
    // build script cannot depend on.
    #[allow(clippy::disallowed_methods)]
    let resolved = std::env::var("MERGIFY_RELEASE_VERSION")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    println!("cargo:rustc-env=MERGIFY_CLI_VERSION={resolved}");
}
