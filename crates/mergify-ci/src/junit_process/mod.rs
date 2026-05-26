//! `mergify ci junit-process` — `JUnit` XML report → OTLP trace
//! upload + quarantine check.
//!
//! The command port lands in three steps so each layer is
//! reviewable on its own:
//!
//! - **Phase A** (landed) — [`junit`]: `JUnit` XML parser
//!   producing semantically-tagged [`TestCase`] values.
//!   Hermetic, no network.
//! - **Phase B** (landed) — [`spans`] turns parser output into an
//!   OTLP `ExportTraceServiceRequest`; [`upload`] gzips that
//!   protobuf payload and POSTs it to
//!   `/v1/repos/<owner>/<repo>/ci/traces`.
//! - **Phase C** (this commit) — [`quarantine`] queries the
//!   quarantine API; [`command::run`] orchestrates everything and
//!   renders the human report so the binary can promote
//!   `Subcommands::Ci(CiSubcommand::JunitProcess)` from shim to
//!   native.

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
