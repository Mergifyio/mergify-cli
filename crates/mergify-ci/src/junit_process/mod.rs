//! `mergify ci junit-process` — `JUnit` XML report → OTLP trace
//! upload + quarantine check.
//!
//! The command port lands in three steps so each layer is
//! reviewable on its own:
//!
//! - **Phase A** (this commit) — [`junit`]: `JUnit` XML parser
//!   producing semantically-tagged [`TestCase`] values.
//!   Hermetic, no network.
//! - **Phase B** (next) — OTLP protobuf encoding + upload.
//! - **Phase C** (final) — quarantine API client, CLI dispatch,
//!   and `Subcommands::Ci(CiSubcommand::JunitProcess)` promotion
//!   from shim to native.
//!
//! Until Phase C lands, the binary keeps shimming
//! `ci junit-process` to Python — but the parser already lives
//! here so subsequent layers have something to consume.

pub mod junit;

pub use junit::{Failure, InvalidJunitXml, ParseResult, TestCase, TestStatus};
