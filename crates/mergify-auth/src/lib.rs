//! `mergify auth` — the Mergify-issued, per-user credential.
//!
//! - [`device`] — the OAuth 2.0 device authorization grant
//!   (RFC 8628) against the Mergify API, which is how the CLI gets
//!   a credential without ever handling a password or a GitHub
//!   token.
//!
//! The credential itself is stored by
//! [`mergify_core::CredentialStore`]; this crate only obtains and
//! revokes it.

pub mod device;
