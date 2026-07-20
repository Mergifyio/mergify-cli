//! Split an oversized OTLP trace request into several uploads that
//! each gzip under [`upload::MAX_GZIPPED_UPLOAD_BYTES`].
//!
//! [`spans::build_traces`] produces one `ExportTraceServiceRequest`
//! for a whole run: a session root span, one suite span per
//! `<testsuite>`, and one case span per `<testcase>`, all sharing a
//! single trace id. A very large `JUnit` report (tens of thousands of
//! cases, or a handful with enormous stack traces) can gzip past the
//! ingest cap. Rather than drop the entire upload, we partition the
//! case spans into several requests.
//!
//! Each chunk is a **self-contained trace**: it carries the session
//! span, the suite spans for the cases it holds, and those cases.
//! Every chunk keeps the original trace id and the original span ids,
//! so the backend reassembles them into one trace exactly as if a
//! single upload had carried them all — repeating the session/suite
//! spans across chunks is an idempotent upsert keyed by
//! `(trace_id, span_id)`. Splitting on the *gzipped* size (the bytes
//! actually sent) is the point of the exercise: gzip ratios vary
//! wildly between a run dominated by short case names and one
//! dominated by megabyte stack traces, so a raw-size heuristic can't
//! predict the compressed body.
//!
//! Each returned [`Chunk`] carries the gzipped bytes it was sized
//! against, so the upload step posts them directly instead of
//! re-compressing.
//!
//! [`spans::build_traces`]: crate::junit_process::spans::build_traces

use std::collections::HashMap;

use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
use prost::Message as _;

use crate::junit_process::upload;

/// One upload: the request plus the gzipped bytes to POST. The bytes
/// are the exact payload the chunk was measured against, so the
/// caller never gzips twice.
#[derive(Debug)]
pub struct Chunk {
    pub request: ExportTraceServiceRequest,
    pub compressed: Vec<u8>,
}

/// Outcome of [`split_request`].
#[derive(Debug)]
pub struct SplitOutcome {
    /// One gzip-under-cap upload per entry, in order. Empty only in
    /// the pathological case where every case is individually
    /// oversized (see `oversized_cases`).
    pub chunks: Vec<Chunk>,
    /// Names of case spans that gzip past the cap on their own — a
    /// single test whose captured output/stack trace is so large it
    /// can't fit in any upload. These are dropped from the upload
    /// (there is nowhere to put them) and reported to the user; the
    /// CI verdict is unaffected because it's computed from the parsed
    /// cases, not the upload.
    pub oversized_cases: Vec<String>,
}

/// Partition `request` into uploads that each gzip to at most `cap`
/// bytes.
///
/// The common case is a small report: the whole request already fits,
/// so a single-element `chunks` is returned after one gzip and the
/// upload path posts that exact payload. Only when the full payload
/// exceeds `cap` do we decompose it and pack the case spans into
/// several requests.
///
/// Returns `Err` only if gzip itself fails (an in-memory
/// `flate2` write, so effectively never) — surfaced rather than
/// swallowed so a compression failure reads as a diagnosable upload
/// error instead of silently dropping every case as "too large".
pub fn split_request(
    request: &ExportTraceServiceRequest,
    cap: usize,
) -> Result<SplitOutcome, std::io::Error> {
    let compressed = gzip_request(request)?;
    if compressed.len() <= cap {
        return Ok(SplitOutcome {
            chunks: vec![Chunk {
                request: request.clone(),
                compressed,
            }],
            oversized_cases: Vec::new(),
        });
    }

    // Decompose the single-resource / single-scope layout that
    // `build_traces` emits. Anything else is unexpected and not
    // structurally splittable here, so fall back to a lone chunk —
    // the upload may be refused, but that's strictly better than
    // panicking, and this branch is unreachable for our own builder.
    let Some(decomposed) = Decomposed::from_request(request) else {
        return Ok(SplitOutcome {
            chunks: vec![Chunk {
                request: request.clone(),
                compressed,
            }],
            oversized_cases: Vec::new(),
        });
    };

    let mut chunks = Vec::new();
    let mut oversized_cases = Vec::new();
    decomposed.pack(&decomposed.cases, cap, &mut chunks, &mut oversized_cases)?;
    Ok(SplitOutcome {
        chunks,
        oversized_cases,
    })
}

/// A case span together with the suite span it hangs off (if any) and
/// its protobuf-encoded size (computed once, reused by
/// [`split_index`]).
struct CaseEntry<'a> {
    case: &'a Span,
    suite: Option<&'a Span>,
    encoded_len: usize,
}

/// The pieces of a built trace we need to reassemble arbitrary
/// subsets of its cases into standalone requests.
struct Decomposed<'a> {
    template: &'a ExportTraceServiceRequest,
    session: &'a Span,
    cases: Vec<CaseEntry<'a>>,
}

impl<'a> Decomposed<'a> {
    fn from_request(request: &'a ExportTraceServiceRequest) -> Option<Self> {
        let [resource_spans] = request.resource_spans.as_slice() else {
            return None;
        };
        let [scope_spans] = resource_spans.scope_spans.as_slice() else {
            return None;
        };
        let spans = &scope_spans.spans;

        // Session is the sole root (empty parent). Suites are its
        // direct children; everything else is a case hanging off a
        // suite. Build a span-id → suite lookup so each case resolves
        // its suite in one pass.
        let session = spans.iter().find(|s| s.parent_span_id.is_empty())?;
        let mut suites: HashMap<&[u8], &Span> = HashMap::new();
        for span in spans {
            if span.parent_span_id == session.span_id && span.span_id != session.span_id {
                suites.insert(span.span_id.as_slice(), span);
            }
        }

        let cases = spans
            .iter()
            .filter(|s| s.span_id != session.span_id && !suites.contains_key(s.span_id.as_slice()))
            .map(|case| CaseEntry {
                case,
                suite: suites.get(case.parent_span_id.as_slice()).copied(),
                encoded_len: case.encoded_len(),
            })
            .collect();

        Some(Self {
            template: request,
            session,
            cases,
        })
    }

    /// Recursively split `cases` until every produced request gzips
    /// under `cap`, appending the uploads to `chunks`. A single case
    /// that can't fit even on its own is recorded in `oversized` and
    /// dropped.
    ///
    /// Each node re-gzips its own slice rather than estimating from a
    /// ratio: gzip is the only exact measure of the wire size, and
    /// this path only runs for the rare report that already exceeds
    /// the cap, where the multi-MiB upload I/O dwarfs the extra
    /// compression. Correctness (never ship an over-cap chunk) is
    /// worth the ~log(chunk-count) re-compressions.
    fn pack(
        &self,
        cases: &[CaseEntry<'a>],
        cap: usize,
        chunks: &mut Vec<Chunk>,
        oversized: &mut Vec<String>,
    ) -> Result<(), std::io::Error> {
        let request = self.assemble(cases);
        let compressed = gzip_request(&request)?;
        if compressed.len() <= cap {
            chunks.push(Chunk {
                request,
                compressed,
            });
            return Ok(());
        }
        if cases.len() <= 1 {
            // The lone case (plus the unavoidable session/suite
            // framing) still overflows — nowhere left to split.
            if let Some(entry) = cases.first() {
                oversized.push(entry.case.name.clone());
            }
            return Ok(());
        }
        let mid = split_index(cases);
        self.pack(&cases[..mid], cap, chunks, oversized)?;
        self.pack(&cases[mid..], cap, chunks, oversized)
    }

    /// Build a standalone request carrying `cases`: the session span,
    /// the distinct suite spans those cases belong to (first-seen
    /// order), then the case spans. Resource and scope metadata are
    /// cloned from the original so the chunk routes identically.
    fn assemble(&self, cases: &[CaseEntry<'a>]) -> ExportTraceServiceRequest {
        let resource_spans = &self.template.resource_spans[0];
        let scope_spans = &resource_spans.scope_spans[0];

        let mut spans = Vec::with_capacity(1 + cases.len());
        spans.push(self.session.clone());

        let mut seen_suites: Vec<&[u8]> = Vec::new();
        for entry in cases {
            if let Some(suite) = entry.suite
                && !seen_suites.contains(&suite.span_id.as_slice())
            {
                seen_suites.push(suite.span_id.as_slice());
                spans.push(suite.clone());
            }
        }
        for entry in cases {
            spans.push(entry.case.clone());
        }

        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: resource_spans.resource.clone(),
                scope_spans: vec![ScopeSpans {
                    scope: scope_spans.scope.clone(),
                    spans,
                    schema_url: scope_spans.schema_url.clone(),
                }],
                schema_url: resource_spans.schema_url.clone(),
            }],
        }
    }
}

/// Pick the index that splits `cases` into two halves of roughly
/// equal *encoded* size, so one giant case is isolated fast instead
/// of dragged through many count-based halvings. The result is always
/// in `1..cases.len()` so both halves are non-empty (caller
/// guarantees `cases.len() >= 2`).
fn split_index(cases: &[CaseEntry]) -> usize {
    let total: usize = cases.iter().map(|e| e.encoded_len).sum();
    let mut acc = 0;
    for (i, entry) in cases.iter().enumerate() {
        acc += entry.encoded_len;
        if acc * 2 >= total {
            return (i + 1).clamp(1, cases.len() - 1);
        }
    }
    // Unreachable for non-empty encoded sizes; keep a sane fallback.
    cases.len() / 2
}

/// gzip a request the same way [`upload::upload`] does. The error is
/// propagated so a compression failure surfaces as a real upload
/// error rather than being mistaken for an oversized payload.
fn gzip_request(request: &ExportTraceServiceRequest) -> Result<Vec<u8>, std::io::Error> {
    upload::gzip(&request.encode_to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::junit_process::junit::{Failure, ParseResult, TestCase, TestStatus};
    use crate::junit_process::spans::{UploadMetadata, build_traces};
    use crate::testing::{incompressible, with_ci_env};
    use std::collections::BTreeSet;
    use std::time::Duration;

    fn case(name: &str, suite: &str, stacktrace_len: usize) -> TestCase {
        // Derive a stable per-case seed from the name so repeated
        // builds are deterministic.
        let seed = name.bytes().fold(1u64, |acc, b| {
            acc.wrapping_mul(31).wrapping_add(u64::from(b))
        });
        TestCase {
            name: name.to_string(),
            suite_name: suite.to_string(),
            duration: Some(Duration::from_secs(0)),
            file: None,
            line: None,
            status: if stacktrace_len > 0 {
                TestStatus::Failed
            } else {
                TestStatus::Passed
            },
            failure: Failure {
                kind: None,
                message: None,
                stacktrace: (stacktrace_len > 0).then(|| incompressible(seed, stacktrace_len)),
            },
        }
    }

    fn build(cases: Vec<TestCase>) -> ExportTraceServiceRequest {
        let parsed = ParseResult {
            suite_names: cases.iter().map(|c| c.suite_name.clone()).collect(),
            cases,
        };
        let metadata = UploadMetadata {
            test_framework: Some("pytest".to_string()),
            test_language: Some("python".to_string()),
            mergify_test_job_name: None,
            quarantined: BTreeSet::new(),
        };
        with_ci_env(&[], || build_traces(&parsed, &metadata)).request
    }

    /// Collect the `test.case.name` of every case span in a chunk.
    fn case_names(chunk: &Chunk) -> Vec<String> {
        spans_with_scope(chunk, "case")
            .map(|s| s.name.clone())
            .collect()
    }

    /// Iterate the spans of a chunk whose `test.scope` attribute
    /// equals `scope` (`session` / `suite` / `case`).
    fn spans_with_scope<'a>(chunk: &'a Chunk, scope: &'a str) -> impl Iterator<Item = &'a Span> {
        use opentelemetry_proto::tonic::common::v1::any_value::Value;
        chunk.request.resource_spans[0].scope_spans[0]
            .spans
            .iter()
            .filter(move |s| {
                s.attributes.iter().any(|kv| {
                    kv.key == "test.scope"
                        && matches!(
                            kv.value.as_ref().and_then(|v| v.value.as_ref()),
                            Some(Value::StringValue(v)) if v == scope
                        )
                })
            })
    }

    #[test]
    fn small_report_stays_a_single_chunk() {
        let request = build(vec![case("t.a", "suite", 0), case("t.b", "suite", 0)]);
        let outcome = split_request(&request, upload::MAX_GZIPPED_UPLOAD_BYTES).unwrap();
        assert_eq!(outcome.chunks.len(), 1);
        assert!(outcome.oversized_cases.is_empty());
        // Byte-identical to the un-split request: the common path must
        // not perturb the payload, and the compressed bytes are the
        // gzip of exactly that request (posted as-is, no re-gzip).
        assert_eq!(
            outcome.chunks[0].request.encode_to_vec(),
            request.encode_to_vec()
        );
        assert_eq!(
            outcome.chunks[0].compressed,
            gzip_request(&request).unwrap()
        );
    }

    #[test]
    fn large_report_splits_into_under_cap_chunks_covering_every_case() {
        // 40 cases × ~2 KiB of stacktrace each. With a 4 KiB cap the
        // whole thing must fan out into several uploads.
        let cases: Vec<TestCase> = (0..40)
            .map(|i| case(&format!("t.case_{i}"), "suite", 2048))
            .collect();
        let request = build(cases);
        let cap = 4 * 1024;

        let outcome = split_request(&request, cap).unwrap();

        assert!(
            outcome.chunks.len() > 1,
            "expected a fan-out, got {} chunk(s)",
            outcome.chunks.len()
        );
        assert!(outcome.oversized_cases.is_empty());

        // Every chunk's stored bytes are under the cap and are the
        // gzip of its own request; each chunk is self-contained (has
        // the session span).
        for chunk in &outcome.chunks {
            assert!(chunk.compressed.len() <= cap, "chunk exceeds cap");
            assert_eq!(chunk.compressed, gzip_request(&chunk.request).unwrap());
            let spans = &chunk.request.resource_spans[0].scope_spans[0].spans;
            assert!(
                spans.iter().any(|s| s.parent_span_id.is_empty()),
                "chunk missing session span"
            );
        }

        // Union of case names across chunks == the original set, no
        // loss and no duplication.
        let mut got: Vec<String> = outcome.chunks.iter().flat_map(case_names).collect();
        got.sort();
        let mut want: Vec<String> = (0..40).map(|i| format!("t.case_{i}")).collect();
        want.sort();
        assert_eq!(got, want);

        // Every chunk carries the same trace id — one trace, many
        // uploads.
        let trace_id = &request.resource_spans[0].scope_spans[0].spans[0].trace_id;
        for chunk in &outcome.chunks {
            for span in &chunk.request.resource_spans[0].scope_spans[0].spans {
                assert_eq!(&span.trace_id, trace_id, "trace id drifted across chunks");
            }
        }
    }

    #[test]
    fn oversized_single_case_is_reported_and_dropped_but_others_upload() {
        // One monster case that can't fit under the cap on its own,
        // alongside two normal ones that must still upload.
        let cap = 4 * 1024;
        let request = build(vec![
            case("t.normal_a", "suite", 256),
            case("t.monster", "suite", 64 * 1024),
            case("t.normal_b", "suite", 256),
        ]);

        let outcome = split_request(&request, cap).unwrap();

        assert_eq!(outcome.oversized_cases, vec!["t.monster".to_string()]);
        for chunk in &outcome.chunks {
            assert!(chunk.compressed.len() <= cap);
        }
        let uploaded: Vec<String> = outcome.chunks.iter().flat_map(case_names).collect();
        assert!(uploaded.contains(&"t.normal_a".to_string()));
        assert!(uploaded.contains(&"t.normal_b".to_string()));
        assert!(
            !uploaded.contains(&"t.monster".to_string()),
            "oversized case must not be uploaded"
        );
    }

    #[test]
    fn cases_from_distinct_suites_keep_their_suite_span_in_each_chunk() {
        // Two suites, enough stacktrace to force a split; whichever
        // suite a case lands in, its suite span must ride along.
        let cases: Vec<TestCase> = (0..20)
            .map(|i| {
                let suite = if i % 2 == 0 { "alpha" } else { "beta" };
                case(&format!("t.case_{i}"), suite, 2048)
            })
            .collect();
        let request = build(cases);
        let cap = 4 * 1024;

        let outcome = split_request(&request, cap).unwrap();
        assert!(outcome.chunks.len() > 1);

        for chunk in &outcome.chunks {
            // Every case's parent suite span is present in the chunk.
            let suite_ids: BTreeSet<&[u8]> = spans_with_scope(chunk, "suite")
                .map(|s| s.span_id.as_slice())
                .collect();
            for case_span in spans_with_scope(chunk, "case") {
                assert!(
                    suite_ids.contains(case_span.parent_span_id.as_slice()),
                    "case {} has no suite span in its chunk",
                    case_span.name
                );
            }
        }
    }
}
