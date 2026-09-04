//! `mergify auth` — the Mergify-issued, per-user credential.
//!
//! - [`device`] — the OAuth 2.0 device authorization grant
//!   (RFC 8628) against the Mergify API, which is how the CLI gets
//!   a credential without ever handling a password or a GitHub
//!   token.
//! - [`identity`] — `GET /v1/user`, the only way to turn a
//!   credential into an account name and the only way to tell a
//!   live one from a revoked one.
//! - [`login`] / [`logout`] / [`status`] — the three commands.
//!
//! The credential itself is stored by
//! [`mergify_core::CredentialStore`]; this crate obtains it,
//! revokes it, and reports on it.

pub mod device;
pub mod identity;
pub mod login;
pub mod logout;
pub mod status;

#[cfg(test)]
mod testing {
    use mergify_core::CredentialStore;

    /// A credential store that cannot reach the developer's real
    /// keychain, rooted in a temporary directory the caller keeps
    /// alive for the length of the test.
    pub fn file_store() -> (tempfile::TempDir, CredentialStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::file_at(dir.path().join("credentials.json"));
        (dir, store)
    }

    /// Run `body` to completion with `MERGIFY_TOKEN` forced to
    /// `value`.
    ///
    /// `temp_env` cannot wrap an `.await`, so the future is driven
    /// inside the closure instead. Without this the wiring that
    /// reads the variable is untestable, and untestable wiring is
    /// wiring a future edit can delete with the suite still green:
    /// asserting on the renderer alone proves only that the renderer
    /// can print a note, never that anything asks it to.
    pub fn with_mergify_token<F: std::future::Future>(value: Option<&str>, body: F) -> F::Output {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        temp_env::with_var("MERGIFY_TOKEN", value, || runtime.block_on(body))
    }
}
