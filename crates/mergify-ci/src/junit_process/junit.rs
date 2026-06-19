//! `JUnit` XML → semantically-tagged test cases.
//!
//! The parser accepts the loose dialect every `JUnit` producer in
//! the wild emits: `<testsuites>` root with nested `<testsuite>`
//! children, a bare `<testsuite>` root, or a `<testsuite>` root
//! that itself has nested `<testsuite>` descendants. Within each
//! suite, every `<testcase>` becomes a [`TestCase`] tagged with
//! its result (pass / skip / fail / error) plus optional
//! exception attributes pulled from a `<failure>` or `<error>`
//! child.
//!
//! This module owns the data model only. Converting [`TestCase`]
//! to OTLP spans + uploading them is `super::upload`'s job.

use std::time::Duration;

use mergify_core::CliError;
use quick_xml::events::Event;
use quick_xml::name::QName;
use quick_xml::reader::Reader;
use serde::Serialize;

/// The four states a `JUnit` `<testcase>` can be in. Drives the
/// `test.case.result.status` attribute on each emitted span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestStatus {
    Passed,
    Skipped,
    Failed,
    Errored,
}

impl TestStatus {
    /// Wire value for `test.case.result.status`. Two of the four
    /// collapse: `<error>` is rendered as `"failed"` because the
    /// backend treats them identically. The Rust enum keeps them
    /// distinct so the UI layer can show different copy.
    #[must_use]
    pub fn status_attr(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Skipped => "skipped",
            Self::Failed | Self::Errored => "failed",
        }
    }

    /// True for `Failed` and `Errored` — the cases the quarantine
    /// API has anything to say about.
    #[must_use]
    pub fn is_failure(self) -> bool {
        matches!(self, Self::Failed | Self::Errored)
    }
}

/// Exception-style metadata pulled from a `<failure>` /
/// `<error>` element. Each field is optional because the `JUnit`
/// "spec" doesn't really exist — most frameworks emit some
/// subset.
///
/// Field names map to the OpenTelemetry `exception.*` attribute
/// keys the Python span builder reads — `kind` → `exception.type`,
/// `message` → `exception.message`, `stacktrace` →
/// `exception.stacktrace`. The serialized JSON keeps the
/// Rust-side names so the bridge has a single canonical shape.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Failure {
    /// `type` XML attribute (e.g. `AssertionError`,
    /// `java.lang.RuntimeException`).
    pub kind: Option<String>,
    /// `message` XML attribute (e.g. `assert 1 == 0`).
    pub message: Option<String>,
    /// Body text of the element — typically the stack trace.
    /// Trimmed of surrounding whitespace.
    pub stacktrace: Option<String>,
}

/// A single test case from a `JUnit` XML file, after parsing.
///
/// `name` is `"<classname>.<name>"` when `classname` is present,
/// or just `<name>` otherwise — same composition Python uses.
/// The duration may be zero when the framework omitted a
/// `time=` attribute.
///
/// `duration` serializes as `Option<f64>` (seconds) — `null` when
/// the framework omitted `time=`, otherwise the floating-point
/// number of seconds. Python's bridge decodes this directly into
/// the existing `time` value the span builder used to read from
/// the XML attribute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TestCase {
    /// Fully-qualified test name: `"<classname>.<name>"` or
    /// `"<name>"` when classname is absent.
    pub name: String,
    /// Name of the parent `<testsuite>`. Mirrors Python's
    /// `suite_name` derived from the suite's `name=` attribute,
    /// defaulting to `"unnamed testsuite"`.
    pub suite_name: String,
    /// Self-reported run duration. `None` when the framework
    /// omitted the `time=` attribute; the upload layer translates
    /// `None` into a zero start-time offset.
    #[serde(serialize_with = "serialize_duration_secs")]
    pub duration: Option<Duration>,
    /// `file=` attribute, when set. Becomes the
    /// `code.filepath` span attribute.
    pub file: Option<String>,
    /// `line=` attribute, when set. Becomes the `code.lineno`
    /// span attribute.
    pub line: Option<String>,
    pub status: TestStatus,
    /// First `<failure>` / `<error>` child's metadata. Empty for
    /// passing and skipped cases; matches Python's
    /// "we only care about the first failure/error" branch.
    pub failure: Failure,
}

// Serde's `serialize_with` callback signature forces `&Option<T>`
// here, even though the function body could equivalently take
// `Option<&T>`. Suppress the lint for this one call site rather
// than restructuring the upstream contract.
#[allow(clippy::ref_option)]
fn serialize_duration_secs<S>(duration: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match duration {
        Some(d) => serializer.serialize_some(&d.as_secs_f64()),
        None => serializer.serialize_none(),
    }
}

/// Top-level parse result. `suites` is a flat list of suite
/// names (in document order, deduplicated by index) for diagnostic
/// printing; the actual span hierarchy is reconstructed in the
/// upload layer using `TestCase::suite_name`.
#[derive(Debug, Clone, Serialize)]
pub struct ParseResult {
    /// Testsuite names in document order — first occurrence wins
    /// for nested suites that share a name. This is what the
    /// upload layer iterates over to build one suite span per
    /// entry; deriving suite order from `cases` doesn't work
    /// because a nested suite's cases appear in the case stream
    /// before the parent suite's *direct* cases, even though the
    /// parent's `<testsuite>` opens first.
    pub suite_names: Vec<String>,
    pub cases: Vec<TestCase>,
}

#[derive(Debug, Clone)]
pub struct InvalidJunitXml {
    pub details: String,
}

impl std::fmt::Display for InvalidJunitXml {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Failed to parse JUnit XML: {details}",
            details = self.details
        )
    }
}

impl std::error::Error for InvalidJunitXml {}

impl From<InvalidJunitXml> for CliError {
    fn from(err: InvalidJunitXml) -> Self {
        // Preserve the typed error as a transparent source instead of
        // flattening it to a string (same Display, stays downcastable).
        Self::Source(Box::new(err))
    }
}

/// Parse a `JUnit` XML document into a flat list of [`TestCase`]s.
///
/// Accepts the same shapes Python does:
/// - `<testsuites><testsuite/>…</testsuites>` (most common — pytest, `JUnit` 5)
/// - bare `<testsuite/>` root
/// - `<testsuite>` root with nested `<testsuite/>` descendants
///
/// Anything else (different root tag, or `<testsuites>` /
/// `<testsuite>` with no nested suites at all) is rejected as
/// "no testsuites or testsuite tag found" — same message Python
/// raises.
///
/// # Errors
///
/// [`InvalidJunitXml`] on XML parse failure or unsupported root
/// shape. Surface this as [`CliError::Generic`] at the CLI
/// boundary (the `From` impl above does the conversion).
pub fn parse(xml: &[u8]) -> Result<ParseResult, InvalidJunitXml> {
    let mut reader = Reader::from_reader(xml);
    // Intentionally do NOT enable `trim_text(true)`. quick-xml
    // emits a fresh Text event after every entity reference; with
    // trimming on, the whitespace between `…` and the next `&gt;`
    // gets stripped, producing concatenated runs like `"->None:"`
    // instead of `"-> None:"`. We only collect text while inside a
    // `<failure>` / `<error>` element (see `append_failure_text`),
    // so stray inter-tag whitespace elsewhere is harmless.

    let mut state = ParserState::default();
    let mut buf: Vec<u8> = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => state.on_start(&e)?,
            Ok(Event::End(e)) => state.on_end(&e),
            Ok(Event::Empty(e)) => {
                // An empty element (`<testcase ... />`) is a
                // Start + immediate End at the parser level. The
                // state machine handles that as "open + close in
                // one step": don't push a stack frame.
                state.on_empty(&e)?;
            }
            Ok(Event::Text(e)) => {
                // In quick-xml 0.40 the parser splits text around
                // entities — `Text` events contain literal runs only,
                // and each `&name;` arrives as a separate
                // `GeneralRef` event below; entity resolution
                // happens in the `GeneralRef` arm.
                //
                // `xml10_content()` (vs the plainer `decode()`) is
                // important on Windows: when git checks fixtures
                // out with `core.autocrlf` enabled the file's line
                // endings are `\r\n`, and the XML 1.0 spec
                // requires those to be normalized to `\n` before
                // they reach element text. Without this, failure
                // stacktraces ship `\r\n` and tests that diff on
                // the assembled stacktrace fail only on Windows.
                let s = e.xml10_content().map_err(|err| InvalidJunitXml {
                    details: format!("invalid UTF-8 in element text: {err}"),
                })?;
                state.append_failure_text(&s);
            }
            Ok(Event::CData(e)) => {
                // CDATA bodies are by definition not entity-escaped,
                // so plain UTF-8 decoding is correct here.
                let text = std::str::from_utf8(e.as_ref()).map_err(|err| InvalidJunitXml {
                    details: format!("invalid UTF-8 in CDATA: {err}"),
                })?;
                state.append_failure_text(text);
            }
            Ok(Event::GeneralRef(e)) => {
                // quick-xml emits a separate `GeneralRef` event for
                // each `&name;` reference. The predefined XML
                // entities we handle (`lt`, `gt`, `amp`, `apos`,
                // `quot`) cover pytest-junit output; numeric
                // character references (`&#NN;` / `&#xNN;`) resolve
                // via `resolve_char_ref`. Unknown DTD entities
                // aren't emitted by `JUnit` producers we care about —
                // fall back to the literal `&name;` so we don't
                // silently drop content.
                if e.is_char_ref() {
                    if let Some(ch) = e.resolve_char_ref().map_err(|err| InvalidJunitXml {
                        details: format!("invalid character reference: {err}"),
                    })? {
                        let mut tmp = [0u8; 4];
                        state.append_failure_text(ch.encode_utf8(&mut tmp));
                    }
                } else {
                    let name = e.decode().map_err(|err| InvalidJunitXml {
                        details: format!("invalid UTF-8 in entity reference: {err}"),
                    })?;
                    let resolved = match name.as_ref() {
                        "lt" => "<",
                        "gt" => ">",
                        "amp" => "&",
                        "apos" => "'",
                        "quot" => "\"",
                        _ => "",
                    };
                    if resolved.is_empty() {
                        state.append_failure_text(&format!("&{name};"));
                    } else {
                        state.append_failure_text(resolved);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(InvalidJunitXml {
                    details: e.to_string(),
                });
            }
            _ => {}
        }
        buf.clear();
    }

    state.finalize()
}

/// Local tag of an XML name. quick-xml gives us either
/// `testsuite` or `ns:testsuite` (XML namespaces); Python's
/// `findall(".//{*}testsuite")` ignores the namespace prefix
/// entirely, so we do the same.
fn local_name<'a>(name: QName<'a>) -> &'a [u8] {
    // `QName::local_name` would return a wrapped type; we want
    // raw bytes here. `into_inner()` (`QName: Deref<Target = [u8]>`
    // via `&'a [u8]`) gives the input-borrowed slice back so the
    // returned `&[u8]` stays tied to `'a`, not the temporary
    // function frame.
    let raw: &'a [u8] = name.into_inner();
    raw.rsplit(|b| *b == b':').next().unwrap_or(raw)
}

/// Decode an attribute value to a `String`, resolving XML entity
/// escapes (`&amp;`, `&lt;`, …). The newer `decode_and_unescape`
/// API takes a `&Reader` so it can apply non-UTF-8 transcoding;
/// `JUnit` XML is UTF-8 in every emitter we care about, so the
/// `unescape_value` shortcut (deprecated for that encoding
/// concern, not for behavior) is the right tool here.
#[allow(deprecated)]
fn attr_value(
    attr: &quick_xml::events::attributes::Attribute<'_>,
) -> Result<String, InvalidJunitXml> {
    attr.unescape_value()
        .map(std::borrow::Cow::into_owned)
        .map_err(|e| InvalidJunitXml {
            details: format!("invalid attribute value: {e}"),
        })
}

#[derive(Default)]
#[allow(clippy::struct_excessive_bools)] // four small flags model the JUnit parser's hand-rolled
// state machine more clearly than packing them behind a single enum / bitflags would.
struct ParserState {
    /// True once we've seen the root and confirmed it's
    /// `<testsuites>` or `<testsuite>`. Anything else gets
    /// rejected before any cases land.
    saw_valid_root: bool,
    /// Stack of currently-open `<testsuite>` names. The
    /// innermost one is the "current suite" for any `<testcase>`
    /// we encounter. We allow nested suites because Python does
    /// (it does `findall(".//{*}testsuite")`).
    suite_stack: Vec<String>,
    /// True while we're inside a `<testcase>`; `in_progress` holds
    /// the partially-built case.
    in_progress: Option<TestCase>,
    /// True while we're inside the first `<failure>` / `<error>`
    /// child of the current testcase — we use this to attribute
    /// text content (the stack trace) to the right place. Python
    /// only keeps the FIRST failure/error per case, so once we've
    /// captured one we stop collecting further metadata.
    in_failure: bool,
    /// Accumulator for the current failure/error element body.
    /// quick-xml emits a *separate* `Text` / `GeneralRef` /
    /// `CData` event each time it hits an entity boundary, so we
    /// can't just overwrite a `stacktrace` field per event —
    /// we'd lose everything before the last entity. Append here,
    /// trim once at the closing tag.
    failure_text_buf: String,
    /// Did we already record a failure / error for the current
    /// testcase? If so, ignore further `<failure>` / `<error>`
    /// siblings (Python's `break` after the first).
    failure_captured: bool,
    /// True once we've seen at least one `<testsuite>` element.
    /// Used by `finalize` to enforce Python's "no testsuites or
    /// testsuite tag found" error when neither is present.
    saw_any_testsuite: bool,
    /// Suite names in the order their `<testsuite>` opened in the
    /// document. Mirrors Python's `findall(".//{*}testsuite")`
    /// which yields ancestors before descendants — relevant when
    /// a nested suite's cases appear in the case stream before the
    /// outer suite's direct cases, but the outer suite's span must
    /// still be emitted first.
    suite_names: Vec<String>,
    output: Vec<TestCase>,
}

impl ParserState {
    /// Append a chunk of failure-body text. Called for every
    /// `Text`, `CData`, and `GeneralRef` event quick-xml emits
    /// between the opening `<failure>` / `<error>` and its
    /// matching close.
    fn append_failure_text(&mut self, chunk: &str) {
        if self.in_failure {
            self.failure_text_buf.push_str(chunk);
        }
    }

    fn on_start(&mut self, e: &quick_xml::events::BytesStart<'_>) -> Result<(), InvalidJunitXml> {
        let name = local_name(e.name());
        if !self.saw_valid_root {
            match name {
                b"testsuites" => {
                    self.saw_valid_root = true;
                    return Ok(());
                }
                b"testsuite" => {
                    self.saw_valid_root = true;
                    self.saw_any_testsuite = true;
                    let suite_name = read_suite_name(e)?;
                    self.suite_names.push(suite_name.clone());
                    self.suite_stack.push(suite_name);
                    return Ok(());
                }
                _ => {
                    return Err(InvalidJunitXml {
                        details: "no testsuites or testsuite tag found".to_string(),
                    });
                }
            }
        }

        match name {
            b"testsuite" => {
                self.saw_any_testsuite = true;
                let suite_name = read_suite_name(e)?;
                self.suite_names.push(suite_name.clone());
                self.suite_stack.push(suite_name);
            }
            b"testcase" => {
                if self.in_progress.is_some() {
                    return Err(InvalidJunitXml {
                        details: "nested <testcase> not allowed".to_string(),
                    });
                }
                let suite_name = self
                    .suite_stack
                    .last()
                    .cloned()
                    .unwrap_or_else(|| "unnamed testsuite".to_string());
                self.in_progress = Some(read_testcase(e, &suite_name)?);
                self.failure_captured = false;
            }
            b"failure" | b"error" => {
                if let Some(tc) = self.in_progress.as_mut()
                    && !self.failure_captured
                {
                    tc.status = if name == b"failure" {
                        TestStatus::Failed
                    } else {
                        TestStatus::Errored
                    };
                    tc.failure = read_failure(e)?;
                    self.in_failure = true;
                    self.failure_captured = true;
                    self.failure_text_buf.clear();
                }
            }
            b"skipped" => {
                if let Some(tc) = self.in_progress.as_mut() {
                    // `<skipped>` wins over `<failure>` only when
                    // the failure wasn't already recorded — Python
                    // checks skipped FIRST in its if-elif chain,
                    // so we mirror that by setting status only if
                    // we haven't captured a failure yet.
                    if !self.failure_captured {
                        tc.status = TestStatus::Skipped;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn on_empty(&mut self, e: &quick_xml::events::BytesStart<'_>) -> Result<(), InvalidJunitXml> {
        let name = local_name(e.name());
        // An empty element is identical to <X></X>; treat as
        // open+close pair without bookkeeping the stack frame.
        // For `<skipped/>` / `<failure/>` / `<error/>` (rare but
        // valid — they appear as plain markers without text), we
        // mirror the work `on_start` does and just skip the
        // matching `on_end` since there's no body to track.
        match name {
            b"testsuite" => {
                self.saw_any_testsuite = true;
                if !self.saw_valid_root {
                    self.saw_valid_root = true;
                }
                // `<testsuite name="…"/>` self-closing — record
                // the name in document order so the upload layer
                // emits a (case-less) suite span for it, matching
                // Python's `findall(".//{*}testsuite")` walk.
                self.suite_names.push(read_suite_name(e)?);
                // Empty testsuite contributes nothing to `cases`.
            }
            b"testcase" => {
                if !self.saw_valid_root {
                    return Err(InvalidJunitXml {
                        details: "<testcase> outside <testsuite>".to_string(),
                    });
                }
                let suite_name = self
                    .suite_stack
                    .last()
                    .cloned()
                    .unwrap_or_else(|| "unnamed testsuite".to_string());
                let tc = read_testcase(e, &suite_name)?;
                self.output.push(tc);
            }
            b"skipped" => {
                if let Some(tc) = self.in_progress.as_mut()
                    && !self.failure_captured
                {
                    tc.status = TestStatus::Skipped;
                }
            }
            b"failure" | b"error" => {
                if let Some(tc) = self.in_progress.as_mut()
                    && !self.failure_captured
                {
                    tc.status = if name == b"failure" {
                        TestStatus::Failed
                    } else {
                        TestStatus::Errored
                    };
                    tc.failure = read_failure(e)?;
                    self.failure_captured = true;
                }
            }
            _ if !self.saw_valid_root => {
                return Err(InvalidJunitXml {
                    details: "no testsuites or testsuite tag found".to_string(),
                });
            }
            _ => {}
        }
        Ok(())
    }

    fn on_end(&mut self, e: &quick_xml::events::BytesEnd<'_>) {
        let name = local_name(e.name());
        match name {
            b"testsuite" => {
                self.suite_stack.pop();
            }
            b"testcase" => {
                if let Some(tc) = self.in_progress.take() {
                    self.output.push(tc);
                }
                self.failure_captured = false;
            }
            b"failure" | b"error" if self.in_failure => {
                // Flush the accumulated body text into the
                // testcase's failure record. Trimming once at the
                // close keeps the wire format identical to
                // Python's `(child.text or "").strip()`.
                let trimmed = self.failure_text_buf.trim();
                if !trimmed.is_empty()
                    && let Some(tc) = self.in_progress.as_mut()
                {
                    tc.failure.stacktrace = Some(trimmed.to_string());
                }
                self.failure_text_buf.clear();
                self.in_failure = false;
            }
            _ => {}
        }
    }

    fn finalize(self) -> Result<ParseResult, InvalidJunitXml> {
        if !self.saw_valid_root || !self.saw_any_testsuite {
            return Err(InvalidJunitXml {
                details: "no testsuites or testsuite tag found".to_string(),
            });
        }
        Ok(ParseResult {
            suite_names: self.suite_names,
            cases: self.output,
        })
    }
}

fn read_suite_name(e: &quick_xml::events::BytesStart<'_>) -> Result<String, InvalidJunitXml> {
    for attr in e.attributes() {
        let attr = attr.map_err(|err| InvalidJunitXml {
            details: format!("invalid attribute: {err}"),
        })?;
        if local_name(attr.key) == b"name" {
            return attr_value(&attr);
        }
    }
    Ok("unnamed testsuite".to_string())
}

fn read_testcase(
    e: &quick_xml::events::BytesStart<'_>,
    suite_name: &str,
) -> Result<TestCase, InvalidJunitXml> {
    let mut classname: Option<String> = None;
    let mut name: Option<String> = None;
    let mut time: Option<f64> = None;
    let mut file: Option<String> = None;
    let mut line: Option<String> = None;
    for attr in e.attributes() {
        let attr = attr.map_err(|err| InvalidJunitXml {
            details: format!("invalid attribute: {err}"),
        })?;
        match local_name(attr.key) {
            b"classname" => classname = Some(attr_value(&attr)?),
            b"name" => name = Some(attr_value(&attr)?),
            b"time" => {
                let raw = attr_value(&attr)?;
                if !raw.is_empty() {
                    time = raw.parse::<f64>().ok();
                }
            }
            b"file" => file = Some(attr_value(&attr)?),
            b"line" => line = Some(attr_value(&attr)?),
            _ => {}
        }
    }
    let name = name.unwrap_or_else(|| "unnamed test".to_string());
    let full_name = match classname {
        Some(c) if !c.is_empty() => format!("{c}.{name}"),
        _ => name,
    };
    Ok(TestCase {
        name: full_name,
        suite_name: suite_name.to_string(),
        duration: time.map(Duration::from_secs_f64),
        file,
        line,
        // Default to "passed" — `<failure>` / `<error>` /
        // `<skipped>` children update it on the way in.
        status: TestStatus::Passed,
        failure: Failure::default(),
    })
}

fn read_failure(e: &quick_xml::events::BytesStart<'_>) -> Result<Failure, InvalidJunitXml> {
    let mut failure = Failure::default();
    for attr in e.attributes() {
        let attr = attr.map_err(|err| InvalidJunitXml {
            details: format!("invalid attribute: {err}"),
        })?;
        match local_name(attr.key) {
            b"type" => failure.kind = Some(attr_value(&attr)?),
            b"message" => failure.message = Some(attr_value(&attr)?),
            _ => {}
        }
    }
    Ok(failure)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(s: &str) -> ParseResult {
        parse(s.as_bytes()).expect("xml parses")
    }

    #[test]
    fn rejects_unknown_root() {
        // Anything other than `<testsuites>` or `<testsuite>` at
        // the root is the same error Python raises:
        // "no testsuites or testsuite tag found".
        let err = parse(b"<unrelated/>").unwrap_err();
        assert!(
            err.details.contains("no testsuites or testsuite tag found"),
            "got: {}",
            err.details,
        );
    }

    #[test]
    fn rejects_empty_xml() {
        let err = parse(b"").unwrap_err();
        assert!(
            err.details.contains("no testsuites or testsuite tag found"),
            "got: {}",
            err.details,
        );
    }

    #[test]
    fn rejects_malformed_xml() {
        // Truncated/invalid XML must surface as `InvalidJunitXml`,
        // not panic.
        let err = parse(b"<testsuites><testsuite").unwrap_err();
        // Either the parser or the EOF check fires; we don't pin
        // the exact message, just that we got an error.
        assert!(!err.details.is_empty());
    }

    #[test]
    fn parses_testsuites_with_passing_and_failing_cases() {
        // Mirrors the live-smoke fixture shape: outer
        // `<testsuites>`, one suite, one pass + one fail with
        // `<failure>` carrying a message attribute and body text.
        let r = parse_str(
            r#"<?xml version="1.0" encoding="utf-8"?>
<testsuites>
  <testsuite name="pytest" tests="2" failures="1">
    <testcase classname="tests.test_func" name="test_success" time="0.000"/>
    <testcase classname="tests.test_func" name="test_failed" time="0.012">
      <failure message="assert 1 == 0">stack trace body</failure>
    </testcase>
  </testsuite>
</testsuites>"#,
        );
        assert_eq!(r.cases.len(), 2);
        assert_eq!(r.cases[0].name, "tests.test_func.test_success");
        assert_eq!(r.cases[0].status, TestStatus::Passed);
        assert!(r.cases[0].failure.message.is_none());

        assert_eq!(r.cases[1].name, "tests.test_func.test_failed");
        assert_eq!(r.cases[1].status, TestStatus::Failed);
        assert_eq!(r.cases[1].failure.message.as_deref(), Some("assert 1 == 0"));
        assert_eq!(
            r.cases[1].failure.stacktrace.as_deref(),
            Some("stack trace body")
        );
        // Duration extracted from `time="0.012"`.
        assert_eq!(r.cases[1].duration, Some(Duration::from_secs_f64(0.012)));
    }

    #[test]
    fn parses_bare_testsuite_root() {
        // Some frameworks emit `<testsuite>` as the root directly.
        // Python supports that via the
        // `root.tag == "testsuite"` branch — we must too.
        let r = parse_str(
            r#"<testsuite name="solo">
  <testcase classname="C" name="t"/>
</testsuite>"#,
        );
        assert_eq!(r.cases.len(), 1);
        assert_eq!(r.cases[0].name, "C.t");
        assert_eq!(r.cases[0].suite_name, "solo");
    }

    #[test]
    fn parses_namespaced_elements() {
        // Some emitters (older Maven Surefire) wrap everything in
        // an XML namespace. Python ignores the prefix via
        // `{*}testsuite`; we must do the same.
        let r = parse_str(
            r#"<j:testsuites xmlns:j="https://example/junit">
  <j:testsuite name="ns">
    <j:testcase classname="C" name="t"/>
  </j:testsuite>
</j:testsuites>"#,
        );
        assert_eq!(r.cases.len(), 1);
        assert_eq!(r.cases[0].name, "C.t");
    }

    #[test]
    fn skipped_status_propagates() {
        let r = parse_str(
            r#"<testsuites>
  <testsuite name="s">
    <testcase classname="C" name="t">
      <skipped/>
    </testcase>
  </testsuite>
</testsuites>"#,
        );
        assert_eq!(r.cases.len(), 1);
        assert_eq!(r.cases[0].status, TestStatus::Skipped);
        assert_eq!(r.cases[0].status.status_attr(), "skipped");
    }

    #[test]
    fn error_distinct_from_failure_but_same_wire_attr() {
        // `<error>` and `<failure>` are different XML elements but
        // both render as `status=failed` on the wire (matches
        // Python's branch). The Rust enum keeps them distinct so
        // future UI can show different copy if needed.
        let r = parse_str(
            r#"<testsuites>
  <testsuite name="s">
    <testcase classname="C" name="errored">
      <error type="RuntimeError" message="boom">stack</error>
    </testcase>
  </testsuite>
</testsuites>"#,
        );
        assert_eq!(r.cases.len(), 1);
        assert_eq!(r.cases[0].status, TestStatus::Errored);
        assert_eq!(r.cases[0].status.status_attr(), "failed");
        assert_eq!(r.cases[0].failure.kind.as_deref(), Some("RuntimeError"));
        assert_eq!(r.cases[0].failure.message.as_deref(), Some("boom"));
    }

    #[test]
    fn only_first_failure_kept() {
        // Mirrors Python's `break` after the first failure/error
        // — multiple `<failure>` siblings keep only the first
        // one's metadata.
        let r = parse_str(
            r#"<testsuites>
  <testsuite name="s">
    <testcase classname="C" name="t">
      <failure message="first">trace1</failure>
      <failure message="second">trace2</failure>
    </testcase>
  </testsuite>
</testsuites>"#,
        );
        assert_eq!(r.cases.len(), 1);
        assert_eq!(r.cases[0].failure.message.as_deref(), Some("first"));
        assert_eq!(r.cases[0].failure.stacktrace.as_deref(), Some("trace1"));
    }

    #[test]
    fn cdata_stacktrace_captured() {
        // Stack traces commonly come through as `<![CDATA[...]]>`
        // to avoid escaping. The CDATA event handler must
        // populate `failure.stacktrace` the same way as plain text.
        let r = parse_str(
            r#"<testsuites>
  <testsuite name="s">
    <testcase classname="C" name="t">
      <failure message="m"><![CDATA[ multi
line trace ]]></failure>
    </testcase>
  </testsuite>
</testsuites>"#,
        );
        assert_eq!(r.cases.len(), 1);
        assert_eq!(
            r.cases[0].failure.stacktrace.as_deref(),
            Some("multi\nline trace"),
        );
    }

    #[test]
    fn testcase_without_classname_uses_bare_name() {
        let r = parse_str(
            r#"<testsuites>
  <testsuite name="s">
    <testcase name="raw_test"/>
  </testsuite>
</testsuites>"#,
        );
        assert_eq!(r.cases.len(), 1);
        assert_eq!(r.cases[0].name, "raw_test");
    }

    #[test]
    fn testsuite_without_name_uses_default() {
        let r = parse_str(
            r#"<testsuites>
  <testsuite>
    <testcase name="t"/>
  </testsuite>
</testsuites>"#,
        );
        assert_eq!(r.cases[0].suite_name, "unnamed testsuite");
    }

    #[test]
    fn nested_testsuites_flatten_to_cases() {
        // A `<testsuite>` root with descendant `<testsuite>` —
        // shape Python supports (`testsuites = [root, *root.findall(".//{*}testsuite")]`).
        // Cases under nested suites must still surface, tagged
        // with the *closest* enclosing suite name.
        let r = parse_str(
            r#"<testsuite name="outer">
  <testsuite name="inner">
    <testcase classname="C" name="nested"/>
  </testsuite>
</testsuite>"#,
        );
        assert_eq!(r.cases.len(), 1);
        assert_eq!(r.cases[0].suite_name, "inner");
        assert_eq!(r.cases[0].name, "C.nested");
    }

    #[test]
    fn parses_live_smoke_fixture() {
        // The fixture used by the `test_junit_process` live smoke
        // test — one passing case, one failing case with a
        // multi-line `<failure>` body. If the parser drifts from
        // what the smoke fixture looks like, the upload layer
        // would silently miss cases or attach the wrong attributes
        // to the wrong span. Pin the shape end-to-end.
        const FIXTURE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<testsuites>
    <testsuite name="pytest" errors="0" failures="1" skipped="0" tests="2" time="0.026"
               timestamp="2024-08-14T12:25:18.210796+02:00" hostname="func-tests">
        <testcase classname="tests.test_func" name="test_success" time="0.000"/>
        <testcase classname="tests.test_func" name="test_failed" time="0.000">
            <failure message="assert 1 == 0">def test_failed() -&gt; None:
                &gt; assert 1 == 0
                E assert 1 == 0

                tests/test_func.py:6: AssertionError
            </failure>
        </testcase>
    </testsuite>
</testsuites>"#;
        let r = parse_str(FIXTURE);
        assert_eq!(r.cases.len(), 2);
        let pass = &r.cases[0];
        let fail = &r.cases[1];
        assert_eq!(pass.name, "tests.test_func.test_success");
        assert_eq!(pass.status, TestStatus::Passed);
        assert_eq!(fail.name, "tests.test_func.test_failed");
        assert_eq!(fail.status, TestStatus::Failed);
        // `&gt;` entity must have been unescaped to `>` — that's
        // the whole point of routing the body through
        // `unescape_value`.
        let stack = fail.failure.stacktrace.as_deref().unwrap_or("");
        assert!(stack.contains("> assert 1 == 0"), "got: {stack:?}");
        assert!(stack.contains("AssertionError"), "got: {stack:?}");
        assert_eq!(fail.failure.message.as_deref(), Some("assert 1 == 0"));
    }

    #[test]
    fn empty_testsuites_rejected() {
        // `<testsuites/>` alone has no suites. Python raises
        // "no testsuites or testsuite tag found" via the
        // `if not testsuites:` check after expansion. We mirror
        // that.
        let err = parse(b"<testsuites></testsuites>").unwrap_err();
        assert!(
            err.details.contains("no testsuites or testsuite tag found"),
            "got: {}",
            err.details,
        );
    }
}
