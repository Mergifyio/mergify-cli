//! `mergify ci junit-process` — `JUnit` XML report → OTLP trace
//! upload + quarantine check.
//!
//! Layers:
//!
//! - [`junit`]: `JUnit` XML parser producing semantically-tagged
//!   [`TestCase`] values. Hermetic, no network.
//! - [`spans`]: turns parser output into an OTLP
//!   `ExportTraceServiceRequest`.
//! - [`upload`]: gzips that protobuf payload and POSTs it to
//!   `/v1/repos/<owner>/<repo>/ci/traces`.
//! - [`quarantine`]: queries the quarantine API to learn which
//!   failing tests are currently quarantined.
//! - [`command::run`]: orchestrates everything and renders the
//!   human-facing report.

pub mod command;
pub mod junit;
pub mod quarantine;
pub mod spans;
pub mod upload;

pub use command::{JunitProcessOptions, run};
pub use junit::{Failure, InvalidJunitXml, ParseResult, TestCase, TestStatus};
pub use quarantine::{QuarantineFailed, QuarantineResult, QuarantinedTests};
pub use spans::{BuiltTraces, UploadMetadata, build_traces};
pub use upload::{UploadError, default_client, upload};
