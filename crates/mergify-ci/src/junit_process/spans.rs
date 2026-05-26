//! `TestCase` → OTLP `ExportTraceServiceRequest`.
//!
//! Mirrors the span layout `mergify_cli/ci/junit_processing/junit.py`
//! produces:
//!
//! - one root **session** span per upload (parent: optional
//!   `MERGIFY_TRACEPARENT`),
//! - one **suite** span per `<testsuite>` (parent: session),
//! - one **case** span per `<testcase>` (parent: suite).
//!
//! All spans share a single OTLP `Resource` carrying the CI
//! environment attributes the backend uses for routing and
//! dashboards (provider, pipeline, run, branch, …). Common
//! attributes (`test.framework`, `test.language`) — the
//! caller-supplied per-upload metadata — get folded into every
//! span on top of its scope-specific attributes.

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value::Value as AnyValueOneof;
use opentelemetry_proto::tonic::common::v1::{AnyValue, InstrumentationScope, KeyValue};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::status::StatusCode;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span, Status};

use crate::detector;
use crate::junit_process::junit::{ParseResult, TestCase, TestStatus};

/// Caller-supplied per-upload metadata. `test_framework` and
/// `test_language` get propagated to every span as attributes;
/// `run_id` is the human-readable identifier surfaced in the CLI
/// output (e.g. `Run ID: <hex>`).
#[derive(Debug, Clone, Default)]
pub struct UploadMetadata {
    pub test_framework: Option<String>,
    pub test_language: Option<String>,
    /// Optional `mergify.test.job.name` attribute set when the
    /// `MERGIFY_TEST_JOB_NAME` env var is present at parse time.
    pub mergify_test_job_name: Option<String>,
    /// Set of test names the quarantine API confirmed are
    /// currently quarantined. Each case span whose name is in this
    /// set gets `cicd.test.quarantined = true`; everything else
    /// defaults to `false`. Pass an empty set when the quarantine
    /// check was skipped (no failures) or failed (network/API
    /// error) — the spans then upload with everything marked
    /// non-quarantined, which is the conservative default.
    pub quarantined: BTreeSet<String>,
}

/// Result of converting a [`ParseResult`] (one or more `JUnit`
/// files) into a wire-ready OTLP request.
#[derive(Debug, Clone)]
pub struct BuiltTraces {
    /// Lowercase hex identifier the CLI prints back to the user.
    /// Same value populates the `test.run.id` resource attribute.
    pub run_id: String,
    pub request: ExportTraceServiceRequest,
}

/// Convert a [`ParseResult`] (the union of every parsed `JUnit`
/// file) into an OTLP `ExportTraceServiceRequest`.
///
/// Random trace and span IDs are produced via [`getrandom::fill`].
/// `now_unix_nanos` and `id_source` exist so tests can pin a
/// deterministic clock and randomness source; production callers
/// use [`build_traces`] which fills them from
/// `SystemTime::now()` and `getrandom`.
#[must_use]
pub fn build_traces(parsed: &ParseResult, metadata: &UploadMetadata) -> BuiltTraces {
    build_traces_with(parsed, metadata, system_now_unix_nanos(), &mut OsRandom)
}

#[allow(clippy::too_many_lines)] // Straight-line span construction; splitting
// would just hide the per-span attribute set behind helper noise that's
// harder to skim than the inline form.
fn build_traces_with(
    parsed: &ParseResult,
    metadata: &UploadMetadata,
    now_unix_nanos: u64,
    rng: &mut dyn RandomBytes,
) -> BuiltTraces {
    let trace_id = rng.bytes16();
    let session_span_id = rng.bytes8();
    let run_id = hex_lower(&session_span_id);

    let resource = build_resource(&run_id, metadata);

    let common_attrs = common_attributes(metadata);

    let mut spans: Vec<Span> = Vec::new();

    // Suite spans are appended after we know each suite's earliest
    // case start (so the suite's start_time covers all its cases).
    // Group the parser's flat case list by `suite_name`, preserving
    // first-seen order so the wire output matches Python's
    // document-order iteration over `<testsuite>` elements.
    let suites = group_by_suite(&parsed.cases);

    let mut session_start_time_unix_nanos = now_unix_nanos;

    for (suite_name, suite_cases) in &suites {
        let suite_span_id = rng.bytes8();
        let mut suite_start_time_unix_nanos = now_unix_nanos;

        for case in suite_cases {
            let case_span_id = rng.bytes8();
            let start_time_unix_nanos = case_start_time(now_unix_nanos, case.duration);
            suite_start_time_unix_nanos = suite_start_time_unix_nanos.min(start_time_unix_nanos);

            let mut attrs = common_attrs.clone();
            attrs.push(kv_string("test.scope", "case"));
            attrs.push(kv_string("test.case.name", &case.name));
            attrs.push(kv_string("code.function.name", &case.name));
            attrs.push(kv_bool(
                "cicd.test.quarantined",
                metadata.quarantined.contains(&case.name),
            ));
            if let Some(file) = &case.file {
                attrs.push(kv_string("code.filepath", file));
            }
            if let Some(line) = &case.line {
                attrs.push(kv_string("code.lineno", line));
            }
            attrs.push(kv_string(
                "test.case.result.status",
                case.status.status_attr(),
            ));

            let status_code = match case.status {
                TestStatus::Passed | TestStatus::Skipped => StatusCode::Ok,
                TestStatus::Failed | TestStatus::Errored => StatusCode::Error,
            };
            if case.status.is_failure() {
                if let Some(kind) = &case.failure.kind {
                    attrs.push(kv_string("exception.type", kind));
                }
                if let Some(msg) = &case.failure.message {
                    attrs.push(kv_string("exception.message", msg));
                }
                if let Some(trace) = &case.failure.stacktrace {
                    attrs.push(kv_string("exception.stacktrace", trace));
                }
            }

            spans.push(Span {
                trace_id: trace_id.to_vec(),
                span_id: case_span_id.to_vec(),
                trace_state: String::new(),
                parent_span_id: suite_span_id.to_vec(),
                flags: 0,
                name: case.name.clone(),
                kind: 0,
                start_time_unix_nano: start_time_unix_nanos,
                end_time_unix_nano: now_unix_nanos,
                attributes: attrs,
                dropped_attributes_count: 0,
                events: Vec::new(),
                dropped_events_count: 0,
                links: Vec::new(),
                dropped_links_count: 0,
                status: Some(Status {
                    message: String::new(),
                    code: status_code.into(),
                }),
            });
        }

        session_start_time_unix_nanos =
            session_start_time_unix_nanos.min(suite_start_time_unix_nanos);

        let mut suite_attrs = common_attrs.clone();
        suite_attrs.push(kv_string("test.case.name", suite_name));
        suite_attrs.push(kv_string("test.scope", "suite"));
        spans.push(Span {
            trace_id: trace_id.to_vec(),
            span_id: suite_span_id.to_vec(),
            trace_state: String::new(),
            parent_span_id: session_span_id.to_vec(),
            flags: 0,
            name: suite_name.clone(),
            kind: 0,
            start_time_unix_nano: suite_start_time_unix_nanos,
            end_time_unix_nano: now_unix_nanos,
            attributes: suite_attrs,
            dropped_attributes_count: 0,
            events: Vec::new(),
            dropped_events_count: 0,
            links: Vec::new(),
            dropped_links_count: 0,
            status: None,
        });
    }

    // Session is the root span. Place it FIRST in the wire vector
    // so the backend has it before any child references it.
    let mut session_attrs = common_attrs.clone();
    session_attrs.push(kv_string("test.scope", "session"));
    let session_span = Span {
        trace_id: trace_id.to_vec(),
        span_id: session_span_id.to_vec(),
        trace_state: String::new(),
        parent_span_id: Vec::new(),
        flags: 0,
        name: "test session".to_string(),
        kind: 0,
        start_time_unix_nano: session_start_time_unix_nanos,
        end_time_unix_nano: now_unix_nanos,
        attributes: session_attrs,
        dropped_attributes_count: 0,
        events: Vec::new(),
        dropped_events_count: 0,
        links: Vec::new(),
        dropped_links_count: 0,
        status: None,
    };
    spans.insert(0, session_span);

    let resource_spans = ResourceSpans {
        resource: Some(resource),
        scope_spans: vec![ScopeSpans {
            scope: Some(InstrumentationScope {
                name: "mergify-cli".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                attributes: Vec::new(),
                dropped_attributes_count: 0,
            }),
            spans,
            schema_url: String::new(),
        }],
        schema_url: String::new(),
    };

    BuiltTraces {
        run_id,
        request: ExportTraceServiceRequest {
            resource_spans: vec![resource_spans],
        },
    }
}

fn group_by_suite(cases: &[TestCase]) -> Vec<(String, Vec<&TestCase>)> {
    // Preserve first-seen order. A `Vec<(K, Vec<&T>)>` linear-scan
    // group-by is fine here — JUnit reports rarely exceed a handful
    // of suites, and we get deterministic iteration for free.
    let mut groups: Vec<(String, Vec<&TestCase>)> = Vec::new();
    for case in cases {
        if let Some(existing) = groups.iter_mut().find(|(name, _)| *name == case.suite_name) {
            existing.1.push(case);
        } else {
            groups.push((case.suite_name.clone(), vec![case]));
        }
    }
    groups
}

fn case_start_time(now_unix_nanos: u64, duration: Option<Duration>) -> u64 {
    // Mirror Python's `now - int(float(time) * 10e9)`. The `10e9`
    // is a long-standing bug in the Python emitter (should be
    // `1e9`), so cases appear ~10× longer on the wire than the
    // JUnit report claims. The Mergify backend interprets the
    // current shape, so we replicate it verbatim — fixing it
    // here would silently change every uploaded dashboard. If
    // the Python side ever fixes the multiplier, mirror the
    // change in both places.
    let Some(d) = duration else {
        return now_unix_nanos;
    };
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let scaled_nanos = (d.as_secs_f64() * 10e9) as u64;
    now_unix_nanos.saturating_sub(scaled_nanos)
}

fn build_resource(run_id: &str, metadata: &UploadMetadata) -> Resource {
    let mut attrs = Vec::new();
    attrs.push(kv_string("test.run.id", run_id));

    if let Some(job_name) = &metadata.mergify_test_job_name {
        attrs.push(kv_string("mergify.test.job.name", job_name));
    }

    if let Some(name) = detector::get_pipeline_name() {
        attrs.push(kv_string("cicd.pipeline.name", &name));
    }
    if let Some(name) = detector::get_job_name() {
        attrs.push(kv_string("cicd.pipeline.task.name", &name));
    }
    if let Some(id) = detector::get_cicd_pipeline_run_id() {
        attrs.push(kv_string("cicd.pipeline.run.id", &id));
    }
    if let Some(url) = detector::get_cicd_pipeline_run_url() {
        attrs.push(kv_string("cicd.pipeline.run.url", &url));
    }
    if let Some(attempt) = detector::get_cicd_pipeline_run_attempt() {
        #[allow(clippy::cast_possible_wrap)]
        attrs.push(kv_int("cicd.pipeline.run.attempt", attempt as i64));
    }
    if let Some(sha) = detector::get_head_sha() {
        attrs.push(kv_string("vcs.ref.head.revision", &sha));
    }
    if let Some(name) = detector::get_head_ref_name() {
        attrs.push(kv_string("vcs.ref.head.name", &name));
    }
    if let Some(name) = detector::get_base_ref_name() {
        attrs.push(kv_string("vcs.ref.base.name", &name));
    }
    if let Some(url) = detector::get_repository_url() {
        attrs.push(kv_string("vcs.repository.url.full", &url));
    }
    if let Some(repo) = detector::get_github_repository() {
        attrs.push(kv_string("vcs.repository.name", &repo));
    }
    if let Some(name) = detector::get_cicd_pipeline_runner_name() {
        attrs.push(kv_string("cicd.pipeline.runner.name", &name));
    }
    if let Some(provider) = detector::get_ci_provider() {
        attrs.push(kv_string("cicd.provider.name", provider.as_str()));
    }

    Resource {
        attributes: attrs,
        dropped_attributes_count: 0,
        entity_refs: Vec::new(),
    }
}

fn common_attributes(metadata: &UploadMetadata) -> Vec<KeyValue> {
    let mut attrs = Vec::new();
    if let Some(framework) = &metadata.test_framework {
        attrs.push(kv_string("test.framework", framework));
    }
    if let Some(language) = &metadata.test_language {
        attrs.push(kv_string("test.language", language));
    }
    attrs
}

fn kv(key: &str, value: AnyValueOneof) -> KeyValue {
    // `..Default::default()` so any future proto-generated fields
    // (e.g. the profiling-signal `key_strindex` already present in
    // 0.32) round-trip as their default without us having to spell
    // them out.
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue { value: Some(value) }),
        ..KeyValue::default()
    }
}

fn kv_string(key: &str, value: &str) -> KeyValue {
    kv(key, AnyValueOneof::StringValue(value.to_string()))
}

fn kv_bool(key: &str, value: bool) -> KeyValue {
    kv(key, AnyValueOneof::BoolValue(value))
}

fn kv_int(key: &str, value: i64) -> KeyValue {
    kv(key, AnyValueOneof::IntValue(value))
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // `write!` on a `String` is infallible; the `_` discards
        // the `Ok` value rather than going through a `format!`
        // round trip that allocates per byte.
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn system_now_unix_nanos() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => u64::try_from(d.as_nanos()).unwrap_or(u64::MAX),
        Err(_) => 0,
    }
}

/// Internal seam for tests: any source of random bytes. The
/// production impl reads from the OS via `getrandom`; tests
/// hand-feed deterministic byte streams.
trait RandomBytes {
    fn bytes8(&mut self) -> [u8; 8];
    fn bytes16(&mut self) -> [u8; 16];
}

struct OsRandom;

impl RandomBytes for OsRandom {
    fn bytes8(&mut self) -> [u8; 8] {
        let mut buf = [0u8; 8];
        getrandom::fill(&mut buf).expect("OS rng available");
        buf
    }
    fn bytes16(&mut self) -> [u8; 16] {
        let mut buf = [0u8; 16];
        getrandom::fill(&mut buf).expect("OS rng available");
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::junit_process::junit::Failure;
    use crate::testing::with_ci_env;

    /// Deterministic byte source for tests. Bytes are consumed
    /// in order. Tests provide enough buffer for the spans they
    /// expect to build; running out of bytes panics so a wrong
    /// span-count assumption is loud rather than silent.
    struct FixedRng {
        bytes: Vec<u8>,
        cursor: usize,
    }
    impl FixedRng {
        fn new(bytes: Vec<u8>) -> Self {
            Self { bytes, cursor: 0 }
        }
        fn take(&mut self, n: usize) -> Vec<u8> {
            let slice = self.bytes[self.cursor..self.cursor + n].to_vec();
            self.cursor += n;
            slice
        }
    }
    impl RandomBytes for FixedRng {
        fn bytes8(&mut self) -> [u8; 8] {
            let mut out = [0u8; 8];
            out.copy_from_slice(&self.take(8));
            out
        }
        fn bytes16(&mut self) -> [u8; 16] {
            let mut out = [0u8; 16];
            out.copy_from_slice(&self.take(16));
            out
        }
    }

    fn sample_parsed() -> ParseResult {
        ParseResult {
            cases: vec![
                TestCase {
                    name: "tests.test_func.test_success".to_string(),
                    suite_name: "pytest".to_string(),
                    duration: Some(Duration::from_secs_f64(0.001)),
                    file: None,
                    line: None,
                    status: TestStatus::Passed,
                    failure: Failure::default(),
                },
                TestCase {
                    name: "tests.test_func.test_failed".to_string(),
                    suite_name: "pytest".to_string(),
                    duration: Some(Duration::from_secs_f64(0.002)),
                    file: Some("tests/test_func.py".to_string()),
                    line: Some("6".to_string()),
                    status: TestStatus::Failed,
                    failure: Failure {
                        kind: None,
                        message: Some("assert 1 == 0".to_string()),
                        stacktrace: Some("trace".to_string()),
                    },
                },
            ],
        }
    }

    #[test]
    fn builds_session_suite_and_case_spans_with_consistent_parent_chain() {
        // 16 bytes for trace_id; 4×8 bytes for session, suite,
        // case-1, case-2 span ids. Distinct fill bytes per region
        // so the assertions below can tell them apart at a glance.
        let mut bytes: Vec<u8> = Vec::with_capacity(16 + 4 * 8);
        bytes.extend(std::iter::repeat_n(0xAA, 16)); // trace_id
        bytes.extend(std::iter::repeat_n(0x11, 8)); // session
        bytes.extend(std::iter::repeat_n(0x22, 8)); // suite
        bytes.extend(std::iter::repeat_n(0x33, 8)); // case-1
        bytes.extend(std::iter::repeat_n(0x44, 8)); // case-2
        let mut rng = FixedRng::new(bytes);

        let now: u64 = 1_700_000_000_000_000_000;
        let metadata = UploadMetadata::default();
        let built = with_ci_env(&[], || {
            build_traces_with(&sample_parsed(), &metadata, now, &mut rng)
        });

        // run_id is the session span id rendered as hex.
        assert_eq!(built.run_id, "1111111111111111");

        let resource_spans = &built.request.resource_spans;
        assert_eq!(resource_spans.len(), 1);
        let scope_spans = &resource_spans[0].scope_spans;
        assert_eq!(scope_spans.len(), 1);
        let spans = &scope_spans[0].spans;
        // 1 session + 1 suite + 2 cases.
        assert_eq!(spans.len(), 4);

        // Session is first; suite reports session as parent; both
        // cases report suite as parent.
        let session = &spans[0];
        assert_eq!(session.name, "test session");
        assert!(session.parent_span_id.is_empty());
        assert_eq!(session.span_id, vec![0x11; 8]);
        assert_eq!(session.trace_id, vec![0xAA; 16]);

        let suite = spans.iter().find(|s| s.name == "pytest").unwrap();
        assert_eq!(suite.parent_span_id, vec![0x11; 8]);
        assert_eq!(suite.span_id, vec![0x22; 8]);

        let cases: Vec<&Span> = spans
            .iter()
            .filter(|s| s.name.starts_with("tests.test_func"))
            .collect();
        assert_eq!(cases.len(), 2);
        for case in &cases {
            assert_eq!(case.parent_span_id, vec![0x22; 8]);
            assert_eq!(case.trace_id, vec![0xAA; 16]);
        }
    }

    #[test]
    fn case_status_maps_to_otlp_status_code() {
        let mut rng = FixedRng::new(vec![0xFF; 256]);
        let now: u64 = 1_700_000_000_000_000_000;
        let metadata = UploadMetadata::default();
        let built = with_ci_env(&[], || {
            build_traces_with(&sample_parsed(), &metadata, now, &mut rng)
        });
        let spans = &built.request.resource_spans[0].scope_spans[0].spans;

        let pass = spans
            .iter()
            .find(|s| s.name == "tests.test_func.test_success")
            .unwrap();
        assert_eq!(
            pass.status.as_ref().unwrap().code,
            i32::from(StatusCode::Ok),
        );

        let fail = spans
            .iter()
            .find(|s| s.name == "tests.test_func.test_failed")
            .unwrap();
        assert_eq!(
            fail.status.as_ref().unwrap().code,
            i32::from(StatusCode::Error),
        );
        // exception.message / .stacktrace are attached to failing
        // cases; passing ones don't get the keys at all.
        let fail_attr_keys: Vec<&str> = fail.attributes.iter().map(|kv| kv.key.as_str()).collect();
        assert!(fail_attr_keys.contains(&"exception.message"));
        assert!(fail_attr_keys.contains(&"exception.stacktrace"));
        let pass_attr_keys: Vec<&str> = pass.attributes.iter().map(|kv| kv.key.as_str()).collect();
        assert!(!pass_attr_keys.contains(&"exception.message"));
    }

    #[test]
    fn case_attributes_include_file_line_and_code_function() {
        let mut rng = FixedRng::new(vec![0xFF; 256]);
        let metadata = UploadMetadata::default();
        let built = with_ci_env(&[], || {
            build_traces_with(&sample_parsed(), &metadata, 0, &mut rng)
        });
        let spans = &built.request.resource_spans[0].scope_spans[0].spans;
        let fail = spans
            .iter()
            .find(|s| s.name == "tests.test_func.test_failed")
            .unwrap();
        let by_key: std::collections::HashMap<&str, &AnyValue> = fail
            .attributes
            .iter()
            .filter_map(|kv| kv.value.as_ref().map(|v| (kv.key.as_str(), v)))
            .collect();
        // file/line straight passthrough from the parser.
        assert!(matches!(
            by_key.get("code.filepath").and_then(|v| v.value.as_ref()),
            Some(AnyValueOneof::StringValue(s)) if s == "tests/test_func.py"
        ));
        assert!(matches!(
            by_key.get("code.lineno").and_then(|v| v.value.as_ref()),
            Some(AnyValueOneof::StringValue(s)) if s == "6"
        ));
        // code.function.name mirrors the test case name.
        assert!(matches!(
            by_key.get("code.function.name").and_then(|v| v.value.as_ref()),
            Some(AnyValueOneof::StringValue(s)) if s == "tests.test_func.test_failed"
        ));
        // cicd.test.quarantined defaults to false on every case;
        // the quarantine layer flips it later (Phase C).
        assert!(matches!(
            by_key
                .get("cicd.test.quarantined")
                .and_then(|v| v.value.as_ref()),
            Some(AnyValueOneof::BoolValue(false))
        ));
    }

    #[test]
    fn resource_attributes_carry_ci_env_when_set() {
        let mut rng = FixedRng::new(vec![0xFF; 256]);
        let metadata = UploadMetadata::default();
        let built = with_ci_env(
            &[
                ("GITHUB_ACTIONS", Some("true")),
                ("GITHUB_REPOSITORY", Some("owner/repo")),
                ("GITHUB_WORKFLOW", Some("CI")),
                ("GITHUB_JOB", Some("build")),
                ("GITHUB_RUN_ID", Some("12345")),
                ("GITHUB_RUN_ATTEMPT", Some("2")),
                ("GITHUB_SHA", Some("abc123")),
                ("GITHUB_REF_NAME", Some("main")),
                ("RUNNER_NAME", Some("runner-1")),
            ],
            || build_traces_with(&sample_parsed(), &metadata, 0, &mut rng),
        );
        let resource = built.request.resource_spans[0].resource.as_ref().unwrap();
        let by_key: std::collections::HashMap<&str, &AnyValue> = resource
            .attributes
            .iter()
            .filter_map(|kv| kv.value.as_ref().map(|v| (kv.key.as_str(), v)))
            .collect();

        for (key, expected) in [
            ("cicd.provider.name", "github_actions"),
            ("cicd.pipeline.name", "CI"),
            ("cicd.pipeline.task.name", "build"),
            ("cicd.pipeline.run.id", "12345"),
            ("vcs.ref.head.revision", "abc123"),
            ("vcs.ref.head.name", "main"),
            ("vcs.repository.name", "owner/repo"),
            ("cicd.pipeline.runner.name", "runner-1"),
        ] {
            assert!(
                matches!(
                    by_key.get(key).and_then(|v| v.value.as_ref()),
                    Some(AnyValueOneof::StringValue(s)) if s == expected
                ),
                "expected resource attr {key} == {expected:?}, got {:?}",
                by_key.get(key),
            );
        }
        // Attempt comes through as int, not string.
        assert!(matches!(
            by_key
                .get("cicd.pipeline.run.attempt")
                .and_then(|v| v.value.as_ref()),
            Some(AnyValueOneof::IntValue(2))
        ));
    }

    #[test]
    fn common_attributes_propagate_to_every_span() {
        let mut rng = FixedRng::new(vec![0xFF; 256]);
        let metadata = UploadMetadata {
            test_framework: Some("pytest".to_string()),
            test_language: Some("python".to_string()),
            mergify_test_job_name: None,
            quarantined: BTreeSet::new(),
        };
        let built = with_ci_env(&[], || {
            build_traces_with(&sample_parsed(), &metadata, 0, &mut rng)
        });
        let spans = &built.request.resource_spans[0].scope_spans[0].spans;
        for span in spans {
            let keys: Vec<&str> = span.attributes.iter().map(|kv| kv.key.as_str()).collect();
            assert!(
                keys.contains(&"test.framework"),
                "span {} missing test.framework: {keys:?}",
                span.name
            );
            assert!(
                keys.contains(&"test.language"),
                "span {} missing test.language: {keys:?}",
                span.name
            );
        }
    }

    #[test]
    fn timestamps_propagate_duration_and_session_envelopes_all_cases() {
        // Cases of durations 0.001s and 0.002s. With the 10× scale
        // Python uses, those become 0.01s and 0.02s in nanos before
        // `now`. The session start must be the earliest case start.
        let mut rng = FixedRng::new(vec![0xFF; 256]);
        let now: u64 = 1_000_000_000_000_000_000;
        let metadata = UploadMetadata::default();
        let built = with_ci_env(&[], || {
            build_traces_with(&sample_parsed(), &metadata, now, &mut rng)
        });
        let spans = &built.request.resource_spans[0].scope_spans[0].spans;
        let session = spans.iter().find(|s| s.name == "test session").unwrap();
        let suite = spans.iter().find(|s| s.name == "pytest").unwrap();
        let earliest_case = spans
            .iter()
            .filter(|s| s.name.starts_with("tests."))
            .map(|s| s.start_time_unix_nano)
            .min()
            .unwrap();
        assert_eq!(session.start_time_unix_nano, earliest_case);
        assert_eq!(suite.start_time_unix_nano, earliest_case);
        // End time is `now` for every span.
        assert_eq!(session.end_time_unix_nano, now);
        assert_eq!(suite.end_time_unix_nano, now);
        for span in spans {
            assert!(
                span.end_time_unix_nano == now,
                "{}: {}",
                span.name,
                span.end_time_unix_nano
            );
        }
    }
}
