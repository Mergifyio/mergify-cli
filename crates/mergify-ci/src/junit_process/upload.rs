//! POST an `ExportTraceServiceRequest` to the Mergify CI Insights
//! traces endpoint as OTLP/HTTP/protobuf with gzip.
//!
//! Mirrors `mergify_cli/ci/junit_processing/upload.py`. The Python
//! version delegates to `opentelemetry-exporter-otlp-proto-http`,
//! which boils down to a single `POST` with three headers:
//!
//! - `Authorization: Bearer <token>`
//! - `Content-Type: application/x-protobuf`
//! - `Content-Encoding: gzip`
//!
//! No retries, no streaming, no SDK lifecycle — small enough to do
//! by hand with `reqwest` so we don't drag in `opentelemetry-otlp`
//! and its tonic dependency. The endpoint
//! (`{api_url}/v1/repos/{repository}/ci/traces`) matches the
//! Python URL byte for byte.

use std::io::Write as _;
use std::time::Duration;

use flate2::Compression;
use flate2::write::GzEncoder;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use prost::Message as _;

#[derive(Debug)]
pub struct UploadError {
    pub status: Option<u16>,
    pub message: String,
}

impl std::fmt::Display for UploadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(status) = self.status {
            write!(
                f,
                "Failed to export span batch code: {status}, reason: {msg}",
                msg = self.message,
            )
        } else {
            write!(f, "Failed to export span batch: {}", self.message)
        }
    }
}

impl std::error::Error for UploadError {}

const ENDPOINT_PATH: &str = "/v1/repos/";
const ENDPOINT_SUFFIX: &str = "/ci/traces";

fn endpoint_url(api_url: &str, repository: &str) -> String {
    // The shape `<api_url>/v1/repos/<owner>/<repo>/ci/traces`
    // matches the Python implementation. The repository segment is
    // pre-validated by `detector::split_owner_repo` at the CLI
    // boundary, so we can interpolate it without further escaping.
    let trimmed = api_url.trim_end_matches('/');
    format!("{trimmed}{ENDPOINT_PATH}{repository}{ENDPOINT_SUFFIX}")
}

fn gzip(bytes: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes)?;
    encoder.finish()
}

/// Encode `request` to OTLP/HTTP/protobuf, gzip it, and POST.
///
/// `client` is passed in so callers can configure timeouts, TLS,
/// or test-time wiremock interception once and reuse it for both
/// the quarantine API and the OTLP endpoint (Phase C will share
/// a single `reqwest::Client`).
pub async fn upload(
    client: &reqwest::Client,
    api_url: &str,
    token: &str,
    repository: &str,
    request: &ExportTraceServiceRequest,
) -> Result<(), UploadError> {
    if request.resource_spans.is_empty() {
        // Match Python's `upload()` short-circuit: no spans, no
        // request. The backend would 400 anyway, so we save a
        // round trip.
        return Ok(());
    }

    let url = endpoint_url(api_url, repository);

    let encoded = request.encode_to_vec();
    let compressed = gzip(&encoded).map_err(|e| UploadError {
        status: None,
        message: format!("failed to gzip OTLP payload: {e}"),
    })?;

    let resp = client
        .post(&url)
        .bearer_auth(token)
        .header("Content-Type", "application/x-protobuf")
        .header("Content-Encoding", "gzip")
        .body(compressed)
        .send()
        .await
        .map_err(|e| UploadError {
            status: None,
            message: e.to_string(),
        })?;

    if resp.status().is_success() {
        return Ok(());
    }
    let status = resp.status().as_u16();
    let body = resp
        .text()
        .await
        .unwrap_or_else(|e| format!("<could not read response body: {e}>"));
    Err(UploadError {
        status: Some(status),
        message: body,
    })
}

/// Build a `reqwest::Client` with a sensible per-request timeout
/// for OTLP uploads. The default `reqwest` timeout is unlimited,
/// which can hang CI for an hour if the backend is slow.
#[must_use]
pub fn default_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("rustls reqwest client builds with default config")
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::GzDecoder;
    use opentelemetry_proto::tonic::resource::v1::Resource;
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
    use std::io::Read as _;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, Request as MockRequest, ResponseTemplate};

    fn sample_request() -> ExportTraceServiceRequest {
        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes: Vec::new(),
                    dropped_attributes_count: 0,
                    entity_refs: Vec::new(),
                }),
                scope_spans: vec![ScopeSpans {
                    scope: None,
                    spans: vec![Span {
                        name: "x".into(),
                        ..Span::default()
                    }],
                    schema_url: String::new(),
                }],
                schema_url: String::new(),
            }],
        }
    }

    #[test]
    fn endpoint_url_matches_python_layout() {
        assert_eq!(
            endpoint_url("https://api.mergify.com", "owner/repo"),
            "https://api.mergify.com/v1/repos/owner/repo/ci/traces"
        );
        // Trailing slash on api_url must not produce a double slash.
        assert_eq!(
            endpoint_url("https://api.mergify.com/", "owner/repo"),
            "https://api.mergify.com/v1/repos/owner/repo/ci/traces"
        );
    }

    #[tokio::test]
    async fn empty_request_skips_http_round_trip() {
        // No spans → no request. If the function tries to POST,
        // it'll fail because the URL is bogus.
        let client = reqwest::Client::new();
        let request = ExportTraceServiceRequest {
            resource_spans: Vec::new(),
        };
        upload(
            &client,
            "http://127.0.0.1:1", // refused if actually hit
            "token",
            "owner/repo",
            &request,
        )
        .await
        .expect("empty request must short-circuit");
    }

    #[tokio::test]
    async fn posts_gzipped_protobuf_to_traces_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/repos/owner/repo/ci/traces"))
            .and(header("Authorization", "Bearer secret"))
            .and(header("Content-Type", "application/x-protobuf"))
            .and(header("Content-Encoding", "gzip"))
            .respond_with(|req: &MockRequest| {
                // Decode the body to assert it's valid gzip-protobuf
                // round-tripping back into the same request shape.
                let mut decoder = GzDecoder::new(req.body.as_slice());
                let mut unzipped = Vec::new();
                decoder
                    .read_to_end(&mut unzipped)
                    .expect("body decompresses");
                let decoded_req = ExportTraceServiceRequest::decode(unzipped.as_slice())
                    .expect("body decodes to OTLP request");
                assert_eq!(decoded_req.resource_spans.len(), 1);
                ResponseTemplate::new(200)
            })
            .mount(&server)
            .await;

        let client = default_client();
        upload(
            &client,
            &server.uri(),
            "secret",
            "owner/repo",
            &sample_request(),
        )
        .await
        .expect("upload succeeds");
    }

    #[tokio::test]
    async fn surfaces_http_error_status_and_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/repos/owner/repo/ci/traces"))
            .respond_with(ResponseTemplate::new(401).set_body_string("bad token"))
            .mount(&server)
            .await;

        let client = default_client();
        let err = upload(
            &client,
            &server.uri(),
            "wrong",
            "owner/repo",
            &sample_request(),
        )
        .await
        .expect_err("401 must surface as UploadError");
        assert_eq!(err.status, Some(401));
        assert!(err.message.contains("bad token"), "got: {}", err.message);
        let rendered = err.to_string();
        // The Display impl is what Python prints; match the wording
        // so existing log scrapers / docs don't drift.
        assert!(
            rendered.contains("code: 401") && rendered.contains("reason: bad token"),
            "got: {rendered}"
        );
    }
}
