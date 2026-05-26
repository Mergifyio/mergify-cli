//! `mergify ci junit-process` — `JUnit` XML report → OTLP trace
//! upload + quarantine check.
//!
//! The command port lands in three steps so each layer is
//! reviewable on its own:
//!
//! - **Phase A** (landed) — [`junit`]: `JUnit` XML parser
//!   producing semantically-tagged [`TestCase`] values.
//!   Hermetic, no network.
//! - **Phase B** (this commit) — [`spans`] turns parser output
//!   into an OTLP `ExportTraceServiceRequest`; [`upload`] gzips
//!   that protobuf payload and POSTs it to
//!   `/v1/repos/<owner>/<repo>/ci/traces`.
//! - **Phase C** (next) — quarantine API client, CLI dispatch,
//!   and `Subcommands::Ci(CiSubcommand::JunitProcess)` promotion
//!   from shim to native.
//!
//! Until Phase C lands, the binary keeps shimming
//! `ci junit-process` to Python — but the parser and uploader
//! already live here so the dispatch layer just needs to wire
//! them together.

pub mod junit;
pub mod spans;
pub mod upload;

pub use junit::{Failure, InvalidJunitXml, ParseResult, TestCase, TestStatus};
pub use spans::{BuiltTraces, UploadMetadata, build_traces};
pub use upload::{UploadError, default_client, upload};
