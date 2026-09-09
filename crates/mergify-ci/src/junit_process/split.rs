//! Split an oversized OTLP trace request into several uploads sized
//! against [`upload::MAX_GZIPPED_UPLOAD_BYTES`] — "against" and not
//! "under", by a bounded margin two paths take deliberately; see
//! [`Chunk`].
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
//! Each returned [`Chunk`] carries the gzipped bytes to post, so the
//! upload step never compresses again. They are the bytes the chunk
//! was sized against unless it was stamped, in which case they are
//! the gzip of the stamped payload — always what goes on the wire.
//!
//! # Declaring completeness
//!
//! Self-containment is what makes a chunk unreadable as a *part*:
//! once ingested it carries the session span and a set of case spans
//! that read as a whole run — the cases it happens to hold, with
//! nothing saying how many there were — so a consumer asking "did
//! this session report a failing test?" gets an answer from the
//! first chunk alone, before the rest of the run has landed. Every
//! chunk of a multi-chunk delivery therefore declares its own
//! position ([`CHUNK_INDEX_ATTRIBUTE`]) and the length of the
//! sequence ([`CHUNK_COUNT_ATTRIBUTE`]) as resource attributes.
//!
//! **Both attributes absent means "a single upload, and it is the
//! whole session".** That is the compatibility hinge, and it is why
//! a lone chunk is left unmarked: a session counts as incomplete only
//! when it declares a total it has not reached, so every client
//! published before this existed keeps counting as complete. Any
//! other convention (a required marker, a `0`-based index colliding
//! with proto defaults) would strip "complete" from every deployed
//! client at once.
//!
//! Strictly, absence means "this client says nothing", and an older
//! one that fanned out says nothing either — MRGFY-9128 carries what
//! it would take to tell the two apart, and the blind spot is spelled
//! out on [`DROPPED_CASES_ATTRIBUTE`], where it bites the same way.
//!
//! `i of n` rather than a "last chunk" flag: neither is knowable
//! until packing has finished, so both need the same after-the-fact
//! stamping pass, and the pair additionally distinguishes a *stalled*
//! delivery (chunk 3 of 5, an hour old) from one that merely hasn't
//! finished arriving.
//!
//! On the resource rather than in an HTTP header, though the header
//! would be cheaper — it would leave both the resource and the
//! session span byte-identical across chunks, and delete the re-gzip
//! pass with them. A header does not survive ingest: the consumer has
//! to answer "is this session complete?" long after the upload,
//! against stored telemetry, so the declaration has to sit in a field
//! that gets persisted. The cost is that one session then presents a
//! different resource attribute set per chunk, which assumes the
//! consumer does not key or dedupe on resource identity.
//!
//! # Declaring loss
//!
//! Reaching the declared total says every upload arrived; it does not
//! say every result did. A case whose own payload fits in no upload
//! is dropped here, and one whose name exceeds
//! [`spans::MAX_TEST_NAME_BYTES`] is dropped before this module ever
//! sees it — so a session can announce itself complete and still be
//! missing tests, biased towards the failing ones, since a megabyte
//! stack trace is what makes a case oversized in the first place.
//!
//! [`DROPPED_CASES_ATTRIBUTE`] carries how many were left out,
//! session-wide, on every chunk — so whichever chunk a consumer holds
//! tells it the session is amputated. Absent means nothing was
//! dropped, the same hinge as above.
//!
//! Nothing else in the payload says it. A dropped case span is the
//! only record of its own failure: the session and suite spans carry
//! no counters, and the run's failure count never leaves this process
//! — it feeds the CI verdict and the printed report, not the wire. So
//! a session that lost a failing case is, without this attribute,
//! byte-for-byte a session where that test never ran.
//!
//! [`spans::build_traces`]: crate::junit_process::spans::build_traces
//! [`spans::MAX_TEST_NAME_BYTES`]: crate::junit_process::spans::MAX_TEST_NAME_BYTES

use std::collections::HashMap;

use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
use prost::Message as _;

use crate::junit_process::spans::kv_int;
use crate::junit_process::upload;

/// Resource attribute carrying this upload's 1-based position in the
/// session's chunk sequence. 1-based so it reads as "3 of 5" to
/// whoever ends up looking at it — not to keep an absent attribute
/// distinguishable from the first chunk, which it is anyway: resource
/// attributes are a repeated field, so absence here is absence from
/// the list and never a zero. That collision is only a risk further
/// downstream, in a consumer that stores the index in a column with a
/// default.
pub const CHUNK_INDEX_ATTRIBUTE: &str = "mergify.test.session.chunk.index";

/// Resource attribute carrying how many uploads the session was
/// split into. A reader has the whole session once it has seen this
/// many distinct chunks of the run.
pub const CHUNK_COUNT_ATTRIBUTE: &str = "mergify.test.session.chunk.count";

/// Resource attribute carrying how many test cases this client left
/// out of the upload entirely, across the whole session.
///
/// The key spells out what it counts because it sits next to
/// [`CHUNK_COUNT_ATTRIBUTE`], and `dropped.count` there reads just as
/// easily as "how many chunks were dropped" — from which a consumer
/// could derive `received + dropped == chunk.count`, a rule that is
/// self-consistent and closes a session while results are missing.
/// That is the misread this whole module exists to prevent, so the
/// unit goes in the key; OTLP spells its own equivalent
/// `dropped_attributes_count` in the same protobuf.
///
/// Omitted rather than stamped as `0` when nothing was lost, on the
/// same compatibility hinge as the chunk markers — and with the same
/// blind spot, which a consumer has to know: absent means "nothing
/// lost" from a client that stamps this, and "unknown" from an older
/// one. Today that gap is the chunk markers' alone — splitting has
/// been released since `2026.7.21.1`, so deployed clients do fan out
/// and say nothing about it, while the other refusal (a name over
/// [`spans::MAX_TEST_NAME_BYTES`]) is on `main` and in no release
/// yet, so no client in the wild drops a case for it. Reading absence
/// as whole is the deliberate trade the hinge makes; separating the
/// two needs a producer version on the resource and a consumer that
/// gates on it, which is MRGFY-9128.
///
/// Session-wide rather than per-chunk on purpose: a consumer decides
/// on the session, and a per-chunk tally would make the answer depend
/// on which chunk it happened to read.
pub const DROPPED_CASES_ATTRIBUTE: &str = "mergify.test.session.dropped_cases.count";

/// Room held back from `cap` while packing, so stamping the
/// completeness attributes afterwards cannot push a packed chunk over
/// the real cap.
///
/// The markers are stamped *after* packing because their values do
/// not exist until the partition is final. At most three ride on one
/// chunk — index, count and a drop tally — for ~145 bytes of protobuf
/// before compression, so a kibibyte is an order
/// of magnitude more than deflate can turn that into — and against
/// the production cap it is 0.005% of a 20 MiB budget that already
/// sits 5 MiB below the server's hard limit. What holds the constant
/// honest is `stamping_overhead_fits_in_the_reserved_headroom`, which
/// measures the gzipped cost of stamping; asserting that chunks land
/// under the cap does not, because on any ordinary fixture they land
/// far below it.
const CHUNK_MARKER_RESERVE_BYTES: usize = 1024;

/// One upload: the request plus the gzipped bytes to post. The bytes
/// are always the gzip of `request` — including after
/// [`stamp_completeness_markers`] rewrites both — so the caller posts
/// them as they are and never gzips twice.
///
/// What they were *sized* against depends on the path. A chunk out of
/// a fan-out is sized against `cap` minus
/// [`CHUNK_MARKER_RESERVE_BYTES`], which is what leaves stamping the
/// room to stay under `cap`. Two paths deliberately do not: a lone
/// case admitted at the hard cap, and a single upload that fits and
/// then declares a loss. Both can land a few dozen bytes past `cap`,
/// which is a client-side target sitting 5 MiB below what ingest
/// actually refuses (see [`upload::MAX_GZIPPED_UPLOAD_BYTES`]).
#[derive(Debug)]
pub struct Chunk {
    pub request: ExportTraceServiceRequest,
    pub compressed: Vec<u8>,
}

/// Outcome of [`split_request`].
#[derive(Debug)]
pub struct SplitOutcome {
    /// One upload per entry, in order, each sized to gzip under the
    /// cap (see [`Chunk`] for what "under" means once the
    /// completeness markers are stamped). Empty only in the
    /// pathological case where every case is individually oversized
    /// (see `oversized_cases`).
    pub chunks: Vec<Chunk>,
    /// Names of case spans that gzip past the cap on their own — a
    /// single test whose captured output/stack trace is so large it
    /// can't fit in any upload. These are dropped from the upload
    /// (there is nowhere to put them) and reported to the user; the
    /// CI verdict is unaffected because it's computed from the parsed
    /// cases, not the upload.
    pub oversized_cases: Vec<String>,
}

/// Partition `request` into uploads sized so their gzipped bodies fit
/// `cap` — see [`Chunk`] for the two paths that overshoot it by a
/// bounded margin.
///
/// A `cap` above [`CHUNK_MARKER_RESERVE_BYTES`] is what makes the
/// partition useful: a fanned-out one is packed to `cap` minus that
/// reserve, so the completeness markers stamped afterwards still fit.
/// Nothing enforces it — a smaller `cap` saturates the packing budget
/// to zero and degrades to one upload per case, or, for a case that
/// overflows `cap` on its own, to dropping it. Degrading beats
/// refusing here, and the tests use tiny caps deliberately. The
/// production caller passes [`upload::MAX_GZIPPED_UPLOAD_BYTES`].
///
/// The common case is a small report: the whole request already fits,
/// so a single-element `chunks` is returned after one gzip and the
/// upload path posts that exact payload. Only when the full payload
/// exceeds `cap` do we decompose it and pack the case spans into
/// several requests.
///
/// `dropped_before_split` is how many cases the span builder already
/// refused on name length. They join the ones dropped here for
/// [`DROPPED_CASES_ATTRIBUTE`], because the two are one fact for a
/// consumer — a result missing from the session — and the report the
/// user reads already merges them into a single list.
///
/// Returns `Err` only if gzip itself fails (an in-memory
/// `flate2` write, so effectively never) — surfaced rather than
/// swallowed so a compression failure reads as a diagnosable upload
/// error instead of silently dropping every case as "too large".
pub fn split_request(
    request: &ExportTraceServiceRequest,
    cap: usize,
    dropped_before_split: usize,
) -> Result<SplitOutcome, std::io::Error> {
    let compressed = gzip_request(request)?;
    if compressed.len() <= cap {
        return single_upload(request, compressed, dropped_before_split);
    }

    // Decompose the single-resource / single-scope layout that
    // `build_traces` emits. Anything else is unexpected and not
    // structurally splittable here, so fall back to a lone chunk —
    // the upload may be refused, but that's strictly better than
    // panicking, and this branch is unreachable for our own builder.
    let Some(decomposed) = Decomposed::from_request(request) else {
        return single_upload(request, compressed, dropped_before_split);
    };

    let mut chunks = Vec::new();
    let mut oversized_cases = Vec::new();
    decomposed.pack(
        &decomposed.cases,
        cap.saturating_sub(CHUNK_MARKER_RESERVE_BYTES),
        cap,
        &mut chunks,
        &mut oversized_cases,
    )?;
    stamp_completeness_markers(&mut chunks, dropped_before_split + oversized_cases.len())?;
    Ok(SplitOutcome {
        chunks,
        oversized_cases,
    })
}

/// The whole run in one upload: the payload fit, or it could not be
/// taken apart. It carries no chunk markers — a single upload *is*
/// the session — but it still declares any cases dropped before it,
/// which is the one thing a lone chunk can be missing.
///
/// When the payload fit, those bytes were sized against the full
/// `cap`, not the packing budget, so stamping a drop count can push
/// them a few dozen bytes past `cap` — the same trade the packer
/// already takes for a lone oversized-but-shippable case, and worth
/// far more than fanning out a report that fits, against a cap that
/// keeps 5 MiB clear of the ingest limit.
///
/// The other entry is the undecomposable payload, which was already
/// over `cap` before anything was stamped; nothing bounds it here and
/// nothing did before. It is unreachable for requests this crate
/// builds — it exists so an unexpected layout ships rather than
/// panics.
fn single_upload(
    request: &ExportTraceServiceRequest,
    compressed: Vec<u8>,
    dropped_cases: usize,
) -> Result<SplitOutcome, std::io::Error> {
    let mut chunks = vec![Chunk {
        request: request.clone(),
        compressed,
    }];
    stamp_completeness_markers(&mut chunks, dropped_cases)?;
    Ok(SplitOutcome {
        chunks,
        oversized_cases: Vec::new(),
    })
}

/// Stamp each chunk with its position in the sequence and the
/// session's dropped-case tally, and re-gzip so the stored bytes stay
/// the ones that will be posted.
///
/// A lone chunk that lost nothing is left untouched — attributes and
/// bytes both — which is the compatibility hinge in the module docs.
/// Re-compressing is otherwise confined to a fan-out, where the
/// multi-MiB upload I/O dwarfs one more gzip per chunk, or to the
/// rare lone upload that did lose a case.
fn stamp_completeness_markers(
    chunks: &mut [Chunk],
    dropped_cases: usize,
) -> Result<(), std::io::Error> {
    // Both conversions are unreachable — neither a partition nor a
    // drop tally that large fits in memory — and both resolve the
    // same way when in doubt: never claim more completeness than we
    // have. Declining to mark leaves a total unstated, which a reader
    // treats as one upload; saturating a drop tally over-declares a
    // loss, which costs a consumer its reduction. Under-declaring
    // either would hand back a "complete" we cannot stand behind.
    let Ok(count) = i64::try_from(chunks.len()) else {
        return Ok(());
    };
    let dropped = i64::try_from(dropped_cases).unwrap_or(i64::MAX);
    if count <= 1 && dropped == 0 {
        return Ok(());
    }

    // The index comes out of a range typed by `count`, so it is an
    // `i64` by construction and this loop needs no second conversion
    // — no second answer to the impossibility decided above.
    for (index, chunk) in (1..=count).zip(chunks.iter_mut()) {
        // Only a request with no `ResourceSpans` at all reaches this
        // skip, and it can only arrive through `single_upload` — the
        // packed path builds every chunk with `assemble`, which
        // always emits exactly one. Such a request carries no spans
        // either, so there is no session to declare anything about;
        // inventing a resource the payload does not have would say
        // less than nothing.
        let Some(resource_spans) = chunk.request.resource_spans.first_mut() else {
            continue;
        };
        let resource = resource_spans
            .resource
            .get_or_insert_with(Resource::default);
        // A lone chunk reaches here only to declare a loss: marking
        // it `1 of 1` would say "multi-chunk" about an upload that is
        // the whole sequence, which is what absence already says.
        if count > 1 {
            resource
                .attributes
                .push(kv_int(CHUNK_INDEX_ATTRIBUTE, index));
            resource
                .attributes
                .push(kv_int(CHUNK_COUNT_ATTRIBUTE, count));
        }
        if dropped > 0 {
            resource
                .attributes
                .push(kv_int(DROPPED_CASES_ATTRIBUTE, dropped));
        }
        chunk.compressed = gzip_request(&chunk.request)?;
    }
    Ok(())
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
    /// Two caps, and the difference matters. `soft_cap` is the
    /// packing target — `cap` minus the room stamping will need — and
    /// it decides when to keep splitting. `hard_cap` is the real
    /// limit, and it decides only whether a case is droppable: a lone
    /// case that overflows the soft cap but fits the hard one is
    /// shipped rather than discarded. Testing the drop against the
    /// soft cap instead would move the "too large to upload"
    /// threshold down by the reserve and silently stop delivering
    /// results this CLI used to deliver — a test's results are worth
    /// far more than the ~100 bytes of overshoot, which the hard
    /// cap's own 5 MiB of headroom below the ingest limit absorbs.
    fn pack(
        &self,
        cases: &[CaseEntry<'a>],
        soft_cap: usize,
        hard_cap: usize,
        chunks: &mut Vec<Chunk>,
        oversized: &mut Vec<String>,
    ) -> Result<(), std::io::Error> {
        let request = self.assemble(cases);
        let compressed = gzip_request(&request)?;
        if compressed.len() <= soft_cap {
            chunks.push(Chunk {
                request,
                compressed,
            });
            return Ok(());
        }
        match cases {
            // Nothing to ship and nothing to report: only the
            // top-level call can arrive here empty, since
            // `split_index` never yields an empty half.
            [] => return Ok(()),
            // The lone case (plus the unavoidable session/suite
            // framing) still overflows — nowhere left to split.
            [entry] => {
                if compressed.len() <= hard_cap {
                    chunks.push(Chunk {
                        request,
                        compressed,
                    });
                } else {
                    oversized.push(entry.case.name.clone());
                }
                return Ok(());
            }
            _ => {}
        }
        let mid = split_index(cases);
        self.pack(&cases[..mid], soft_cap, hard_cap, chunks, oversized)?;
        self.pack(&cases[mid..], soft_cap, hard_cap, chunks, oversized)
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

    /// Read an integer resource attribute off a chunk, or `None`
    /// when the chunk doesn't carry it.
    fn resource_int(chunk: &Chunk, key: &str) -> Option<i64> {
        use opentelemetry_proto::tonic::common::v1::any_value::Value;
        chunk.request.resource_spans[0]
            .resource
            .as_ref()?
            .attributes
            .iter()
            .find(|kv| kv.key == key)
            .and_then(|kv| match kv.value.as_ref()?.value.as_ref()? {
                Value::IntValue(v) => Some(*v),
                _ => None,
            })
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
        let outcome = split_request(&request, upload::MAX_GZIPPED_UPLOAD_BYTES, 0).unwrap();
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

        let outcome = split_request(&request, cap, 0).unwrap();

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

        let outcome = split_request(&request, cap, 0).unwrap();

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
    fn single_chunk_carries_no_completeness_marker() {
        let request = build(vec![case("t.a", "suite", 0), case("t.b", "suite", 0)]);
        let outcome = split_request(&request, upload::MAX_GZIPPED_UPLOAD_BYTES, 0).unwrap();
        assert_eq!(outcome.chunks.len(), 1);
        assert_eq!(
            resource_int(&outcome.chunks[0], CHUNK_INDEX_ATTRIBUTE),
            None
        );
        assert_eq!(
            resource_int(&outcome.chunks[0], CHUNK_COUNT_ATTRIBUTE),
            None
        );
        // Nothing was lost either, so the third marker is absent too:
        // this payload is what every deployed client sends, and all
        // three absences have to keep meaning "whole".
        assert_eq!(
            resource_int(&outcome.chunks[0], DROPPED_CASES_ATTRIBUTE),
            None
        );
    }

    #[test]
    fn unsplittable_request_shape_stays_unmarked() {
        // `Decomposed::from_request` bails on any layout our builder
        // doesn't emit, and the lone fallback chunk is the whole
        // payload. `cap: 0` forces the over-cap branch without needing
        // a multi-MiB fixture.
        let mut request = build(vec![case("t.a", "suite", 0)]);
        // Two resource_spans is not the single-resource layout
        // `build_traces` produces, so decomposition declines.
        let extra = request.resource_spans[0].clone();
        request.resource_spans.push(extra);

        let outcome = split_request(&request, 0, 0).unwrap();

        assert_eq!(outcome.chunks.len(), 1);
        assert!(outcome.oversized_cases.is_empty());
        assert_eq!(
            resource_int(&outcome.chunks[0], CHUNK_INDEX_ATTRIBUTE),
            None
        );
        assert_eq!(
            resource_int(&outcome.chunks[0], CHUNK_COUNT_ATTRIBUTE),
            None
        );
    }

    #[test]
    fn split_chunks_declare_their_position_and_stay_under_cap() {
        let cases: Vec<TestCase> = (0..40)
            .map(|i| case(&format!("t.case_{i}"), "suite", 2048))
            .collect();
        let request = build(cases);
        let cap = 4 * 1024;

        let outcome = split_request(&request, cap, 0).unwrap();
        let total = i64::try_from(outcome.chunks.len()).unwrap();
        assert!(total > 1, "expected a fan-out, got {total} chunk(s)");

        for (position, chunk) in outcome.chunks.iter().enumerate() {
            let index = i64::try_from(position).unwrap() + 1;
            assert_eq!(
                resource_int(chunk, CHUNK_INDEX_ATTRIBUTE),
                Some(index),
                "chunk {index} misdeclares its position"
            );
            assert_eq!(
                resource_int(chunk, CHUNK_COUNT_ATTRIBUTE),
                Some(total),
                "chunk {index} misdeclares the sequence length"
            );
            // The reserve exists so stamping can't push a packed
            // chunk past the cap. Measure the stamped bytes rather
            // than trusting the constant, and check the stored body
            // is the one that will actually be POSTed.
            assert_eq!(chunk.compressed, gzip_request(&chunk.request).unwrap());
            assert!(
                chunk.compressed.len() <= cap,
                "chunk {index} exceeds the cap once marked: {} > {cap}",
                chunk.compressed.len()
            );
            // Nothing was dropped here, so nothing claims a loss:
            // absence is the whole convention, and stamping a `0`
            // instead would say "amputated by nothing" on the most
            // ordinary fan-out there is.
            assert_eq!(
                resource_int(chunk, DROPPED_CASES_ATTRIBUTE),
                None,
                "chunk {index} declared a loss that never happened"
            );
        }
    }

    #[test]
    fn marker_keys_are_pinned_in_the_mergify_namespace() {
        // These strings are the wire contract the engine reads;
        // renaming the constant is free, renaming its value is a
        // protocol break. `mergify.` is where this builder already
        // files its vendor-only concepts (`mergify.test.job.name`),
        // which is what a chunk total is.
        assert_eq!(CHUNK_INDEX_ATTRIBUTE, "mergify.test.session.chunk.index");
        assert_eq!(CHUNK_COUNT_ATTRIBUTE, "mergify.test.session.chunk.count");
        assert_eq!(
            DROPPED_CASES_ATTRIBUTE,
            "mergify.test.session.dropped_cases.count"
        );
    }

    #[test]
    fn a_fan_out_declares_the_cases_it_could_not_carry() {
        // Reaching the declared total says every upload arrived, not
        // that every result did: the monster case fits in no upload
        // and is dropped. Without a count, that session announces
        // itself whole while missing a test — and the missing ones
        // skew towards failures, which is what the reduction would
        // then be answering over.
        let cap = 4 * 1024;
        let request = build(vec![
            case("t.normal_a", "suite", 2048),
            case("t.monster", "suite", 64 * 1024),
            case("t.normal_b", "suite", 2048),
            case("t.normal_c", "suite", 2048),
        ]);

        let outcome = split_request(&request, cap, 0).unwrap();

        assert_eq!(outcome.oversized_cases, vec!["t.monster".to_string()]);
        assert!(
            outcome.chunks.len() > 1,
            "fixture must fan out for this test to say anything"
        );
        for chunk in &outcome.chunks {
            // On every chunk, not just one: a consumer holding any
            // single upload has to be able to tell.
            assert_eq!(
                resource_int(chunk, DROPPED_CASES_ATTRIBUTE),
                Some(1),
                "a chunk carried no loss count"
            );
            assert_eq!(chunk.compressed, gzip_request(&chunk.request).unwrap());
        }
    }

    #[test]
    fn a_lone_upload_missing_a_case_declares_the_loss_and_no_sequence() {
        // The span builder refuses an over-long test name before the
        // split runs, so a report that fits in one upload can still
        // be missing results. The loss is declared; the chunk markers
        // are not, because one upload is the whole sequence.
        let request = build(vec![case("t.a", "suite", 0)]);

        let outcome = split_request(&request, upload::MAX_GZIPPED_UPLOAD_BYTES, 2).unwrap();

        assert_eq!(outcome.chunks.len(), 1);
        let chunk = &outcome.chunks[0];
        assert_eq!(resource_int(chunk, DROPPED_CASES_ATTRIBUTE), Some(2));
        assert_eq!(resource_int(chunk, CHUNK_INDEX_ATTRIBUTE), None);
        assert_eq!(resource_int(chunk, CHUNK_COUNT_ATTRIBUTE), None);
        // Stamped after the sizing gzip, so the stored bytes must
        // have been recomputed — they are the ones posted.
        assert_eq!(chunk.compressed, gzip_request(&chunk.request).unwrap());
    }

    #[test]
    fn losses_from_both_causes_are_added_up() {
        // The two refusals — a name over the limit, a payload over
        // the cap — are independent, and the report most likely to
        // hit both at once is exactly the one that fans out: a huge
        // run with megabyte stack traces. Taking either side alone
        // (a max, a forgotten addend) under-counts the amputation on
        // the one shape this attribute exists for.
        let cap = 4 * 1024;
        let request = build(vec![
            case("t.normal_a", "suite", 2048),
            case("t.monster_a", "suite", 64 * 1024),
            case("t.normal_b", "suite", 2048),
            case("t.monster_b", "suite", 64 * 1024),
        ]);

        // Both addends above one, so a tally that collapsed either
        // side to "some were lost" would show here. The consumer acts
        // on how amputated a session is, not on whether it is.
        let outcome = split_request(&request, cap, 3).unwrap();

        assert_eq!(outcome.oversized_cases.len(), 2);
        for chunk in &outcome.chunks {
            assert_eq!(
                resource_int(chunk, DROPPED_CASES_ATTRIBUTE),
                Some(5),
                "3 refused on name length + 2 refused on size = 5"
            );
        }
    }

    #[test]
    fn every_chunk_of_a_split_is_a_distinct_payload() {
        // The backend registers arrivals by payload hash
        // (`span_test_summary.processed_chunk_hashes`, MRGFY-9104), so
        // two identical bodies would collapse into one and a session
        // could never reach its declared total. Distinctness comes
        // from `pack` giving each chunk a different set of cases —
        // this pins that, not the markers; the distinct index each
        // chunk carries is a second line of defence, not the one
        // under test here.
        let cases: Vec<TestCase> = (0..40)
            .map(|i| case(&format!("t.case_{i}"), "suite", 2048))
            .collect();
        let outcome = split_request(&build(cases), 4 * 1024, 0).unwrap();
        assert!(outcome.chunks.len() > 1);

        let distinct: BTreeSet<&[u8]> = outcome
            .chunks
            .iter()
            .map(|c| c.compressed.as_slice())
            .collect();
        assert_eq!(
            distinct.len(),
            outcome.chunks.len(),
            "two chunks share a payload; the backend would count them once"
        );
    }

    #[test]
    fn the_session_span_stays_byte_identical_across_chunks() {
        // The module's idempotent-upsert promise — every chunk
        // re-sends the same session span, keyed `(trace_id, span_id)`
        // — is why repeating it across uploads is safe. The
        // completeness markers ride on the *resource* precisely so
        // that promise survives: a per-chunk-varying session-span
        // attribute would make whichever upload lands last decide the
        // session's value.
        let cases: Vec<TestCase> = (0..40)
            .map(|i| case(&format!("t.case_{i}"), "suite", 2048))
            .collect();
        let outcome = split_request(&build(cases), 4 * 1024, 0).unwrap();
        assert!(outcome.chunks.len() > 1);

        let encoded: BTreeSet<Vec<u8>> = outcome
            .chunks
            .iter()
            .map(|chunk| {
                spans_with_scope(chunk, "session")
                    .next()
                    .expect("every chunk carries the session span")
                    .encode_to_vec()
            })
            .collect();
        assert_eq!(encoded.len(), 1, "the session span drifted across chunks");
    }

    #[test]
    fn a_lone_case_between_the_packing_budget_and_the_cap_still_ships() {
        // The reserve narrows what `pack` will accept, and it must not
        // narrow what counts as undeliverable with it: a case that
        // cannot be split further but does fit the real cap has to
        // ship. Testing the drop against the packing budget instead
        // would move the "too large to upload" threshold down by the
        // reserve and stop delivering results this CLI used to
        // deliver — for the sake of ~40 bytes of stamping, against a
        // cap that already keeps 5 MiB clear of the ingest limit.
        let request = build(vec![
            case("t.case_a", "suite", 4096),
            case("t.case_b", "suite", 4096),
        ]);
        // Size a lone case's own chunk exactly as `pack` would, then
        // set the cap just above it — so each case clears the cap and
        // fails the packing budget, which is the band at issue.
        let decomposed = Decomposed::from_request(&request).expect("built layout decomposes");
        let lone = gzip_request(&decomposed.assemble(&decomposed.cases[..1]))
            .unwrap()
            .len();
        let cap = lone + 16;
        assert!(
            cap > CHUNK_MARKER_RESERVE_BYTES,
            "fixture must leave a real packing budget"
        );

        let outcome = split_request(&request, cap, 0).unwrap();

        assert!(
            outcome.oversized_cases.is_empty(),
            "a case that fits the cap was dropped: {:?}",
            outcome.oversized_cases
        );
        let mut names: Vec<String> = outcome.chunks.iter().flat_map(case_names).collect();
        names.sort();
        assert_eq!(names, vec!["t.case_a".to_string(), "t.case_b".to_string()]);

        // Chunks taking this escape are stamped like any other, so
        // they can end up a few dozen bytes past `cap`. That overshoot
        // is the deliberate trade, and it is bounded by the reserve.
        for chunk in &outcome.chunks {
            assert!(
                chunk.compressed.len() <= cap + CHUNK_MARKER_RESERVE_BYTES,
                "overshoot beyond the reserved headroom: {} > {}",
                chunk.compressed.len(),
                cap + CHUNK_MARKER_RESERVE_BYTES
            );
        }
    }

    #[test]
    fn a_lone_packed_chunk_is_left_unmarked() {
        // Packing can land on a single chunk: `assemble` reorders the
        // spans (session, then suites, then cases) and that layout can
        // gzip smaller than the one `build_traces` emitted. The result
        // is still the whole session, so it must read as unmarked like
        // any other single upload.
        let request = build(vec![case("t.a", "suite", 0)]);
        let compressed = gzip_request(&request).unwrap();
        let mut chunks = vec![Chunk {
            request,
            compressed,
        }];

        stamp_completeness_markers(&mut chunks, 0).unwrap();

        assert_eq!(resource_int(&chunks[0], CHUNK_INDEX_ATTRIBUTE), None);
        assert_eq!(resource_int(&chunks[0], CHUNK_COUNT_ATTRIBUTE), None);
    }

    #[test]
    fn stamping_overhead_fits_in_the_reserved_headroom() {
        // What `CHUNK_MARKER_RESERVE_BYTES` claims is that stamping a
        // packed chunk cannot grow its gzipped body by more than the
        // reserve — that claim is about deflate, so measure it rather
        // than reason about it. Asserting the chunks land under the
        // cap does not cover this: on any ordinary fixture they land
        // far below it, so the reserve could be zero and nothing
        // would notice until a report packed right up to the line.
        let cases: Vec<TestCase> = (0..40)
            .map(|i| case(&format!("t.case_{i}"), "suite", 2048))
            .collect();
        // Stamped with all three markers — a fan-out that also lost a
        // case is the widest stamp there is, so it is the one the
        // reserve has to cover.
        let outcome = split_request(&build(cases), 4 * 1024, 7).unwrap();
        assert!(outcome.chunks.len() > 1);

        let mut largest_overhead = 0usize;
        for chunk in &outcome.chunks {
            let mut bare = chunk.request.clone();
            let attributes = &mut bare.resource_spans[0]
                .resource
                .as_mut()
                .expect("built chunks carry a resource")
                .attributes;
            let before = attributes.len();
            attributes.retain(|kv| {
                kv.key != CHUNK_INDEX_ATTRIBUTE
                    && kv.key != CHUNK_COUNT_ATTRIBUTE
                    && kv.key != DROPPED_CASES_ATTRIBUTE
            });
            assert_eq!(before - attributes.len(), 3, "all markers must be present");

            let overhead = chunk
                .compressed
                .len()
                .saturating_sub(gzip_request(&bare).unwrap().len());
            largest_overhead = largest_overhead.max(overhead);
        }

        assert!(
            largest_overhead > 0,
            "measured no overhead at all — the comparison is not exercising the markers"
        );
        assert!(
            largest_overhead <= CHUNK_MARKER_RESERVE_BYTES,
            "stamping costs {largest_overhead} gzipped bytes, more than the \
             {CHUNK_MARKER_RESERVE_BYTES} held back while packing"
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

        let outcome = split_request(&request, cap, 0).unwrap();
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
